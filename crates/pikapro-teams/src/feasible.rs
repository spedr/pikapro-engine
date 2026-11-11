//! Whether the team rules leave room for a whole team: checked once, when a
//! generator is built, so that impossible [`Options`](crate::Options) are
//! an error instead of a draw loop that never ends.

use std::collections::BTreeMap;

use pikapro_data::ItemId;

/// What a Pokédex number's sets can hold, for the team rules.
#[derive(Clone, Debug, Default)]
pub(crate) struct NumberItems {
    /// Every set of the number is of a restricted species.
    pub restricted: bool,
    /// Some set can go without an item from `items`: an item-less set, or
    /// (uniform) a forme that needs no particular item.
    pub free: bool,
    /// The items its sets hold otherwise.
    pub items: Vec<ItemId>,
}

/// A max-flow network with unit and bounded capacities.
struct Flow {
    to: Vec<usize>,
    cap: Vec<usize>,
    adj: Vec<Vec<usize>>,
}

impl Flow {
    fn new(nodes: usize) -> Flow {
        Flow { to: Vec::new(), cap: Vec::new(), adj: vec![Vec::new(); nodes] }
    }

    fn edge(&mut self, a: usize, b: usize, cap: usize) {
        self.adj[a].push(self.to.len());
        self.to.push(b);
        self.cap.push(cap);
        self.adj[b].push(self.to.len());
        self.to.push(a);
        self.cap.push(0);
    }

    /// Pushes one unit from `s` to `t` along a path with room (a DFS).
    fn augment(&mut self, s: usize, t: usize) -> bool {
        let mut seen = vec![false; self.adj.len()];
        let mut stack = vec![(s, 0usize)];
        let mut path: Vec<usize> = Vec::new();
        seen[s] = true;
        while let Some(&mut (node, ref mut next)) = stack.last_mut() {
            if node == t {
                for &e in &path {
                    self.cap[e] -= 1;
                    self.cap[e ^ 1] += 1;
                }
                return true;
            }
            match self.adj[node].get(*next).copied() {
                Some(e) => {
                    *next += 1;
                    let b = self.to[e];
                    if self.cap[e] > 0 && !seen[b] {
                        seen[b] = true;
                        path.push(e);
                        stack.push((b, 0));
                    }
                }
                None => {
                    stack.pop();
                    path.pop();
                }
            }
        }
        false
    }
}

/// The most Pokémon (at most `want`) a team can have: one per Pokédex
/// number (Species Clause), distinct items under Item Clause (a `free`
/// number needs none), at most `limit` restricted numbers. A max flow:
/// source → {unrestricted, restricted (capacity `limit`)} → number → item
/// (capacity 1) or "no item" → sink.
pub(crate) fn max_team(numbers: &[NumberItems], item_clause: bool, limit: Option<u8>, want: usize) -> usize {
    const S: usize = 0;
    const T: usize = 1;
    const OPEN: usize = 2;
    const RESTRICTED: usize = 3;
    const NO_ITEM: usize = 4;
    let first = 5;
    let mut items: BTreeMap<usize, usize> = BTreeMap::new();
    for n in numbers.iter().filter(|n| item_clause && !n.free) {
        for i in &n.items {
            let next = first + numbers.len() + items.len();
            items.entry(i.index()).or_insert(next);
        }
    }
    let mut flow = Flow::new(first + numbers.len() + items.len());
    flow.edge(S, OPEN, want);
    flow.edge(S, RESTRICTED, limit.map_or(want, usize::from));
    flow.edge(NO_ITEM, T, want);
    for &node in items.values() {
        flow.edge(node, T, 1);
    }
    for (k, n) in numbers.iter().enumerate() {
        let node = first + k;
        flow.edge(if n.restricted { RESTRICTED } else { OPEN }, node, 1);
        if !item_clause || n.free {
            flow.edge(node, NO_ITEM, 1);
        } else {
            for i in &n.items {
                flow.edge(node, items[&i.index()], 1);
            }
        }
    }
    let mut total = 0;
    while total < want && flow.augment(S, T) {
        total += 1;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number(restricted: bool, free: bool, items: &[u16]) -> NumberItems {
        NumberItems { restricted, free, items: items.iter().map(|&i| ItemId(i)).collect() }
    }

    #[test]
    fn counts_what_the_rules_allow() {
        // Three numbers that all need item 1, one that needs 1 or 2.
        let ns = [number(false, false, &[1]), number(false, false, &[1]), number(false, false, &[1, 2])];
        assert_eq!(max_team(&ns, true, None, 6), 2);
        assert_eq!(max_team(&ns, false, None, 6), 3);
        // Restricted numbers, at most one.
        let ns = [number(true, true, &[]), number(true, true, &[]), number(false, true, &[])];
        assert_eq!(max_team(&ns, true, Some(1), 6), 2);
        assert_eq!(max_team(&ns, true, Some(0), 6), 1);
        assert_eq!(max_team(&ns, true, None, 2), 2);
        // An augmenting path that moves an earlier number to another item.
        let ns = [number(false, false, &[1, 2]), number(false, false, &[1])];
        assert_eq!(max_team(&ns, true, None, 6), 2);
    }
}
