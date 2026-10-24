//! Conversion registry and route planner (MASTER_PROMPT §9.4).
//!
//! Formats are nodes, converters are edges. The planner runs Dijkstra over a
//! lexicographic cost: fidelity loss first, then ownership, then estimated time.
use crate::error::{Error, ErrorCode, Result};
use crate::format::{FormatDescriptor, FormatId, Ownership};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeKind {
    /// Packet copy without decoding.
    StreamCopy,
    Transcode,
}

#[derive(Debug, Clone)]
pub struct Edge {
    pub from: FormatId,
    pub to: FormatId,
    pub kind: EdgeKind,
    /// Fidelity loss, 0 = lossless. Compared first.
    pub loss: u32,
    pub ownership: Ownership,
    /// Estimated time cost, compared last.
    pub time: u32,
    pub description: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Cost {
    loss: u32,
    ownership: u8,
    time: u32,
}

impl Cost {
    const ZERO: Cost = Cost {
        loss: 0,
        ownership: 0,
        time: 0,
    };
    fn plus(&self, e: &Edge) -> Cost {
        Cost {
            loss: self.loss.saturating_add(e.loss),
            ownership: self.ownership.max(e.ownership as u8),
            time: self.time.saturating_add(e.time),
        }
    }
}

/// A planned route: the ordered edges and a human-readable explanation.
#[derive(Debug, Clone)]
pub struct Route {
    pub edges: Vec<Edge>,
}

impl Route {
    pub fn explain(&self) -> String {
        self.edges
            .iter()
            .map(|e| e.description)
            .collect::<Vec<_>>()
            .join(" -> ")
    }
    pub fn total_loss(&self) -> u32 {
        self.edges.iter().map(|e| e.loss).sum()
    }
    /// The weakest ownership level along the route.
    pub fn ownership(&self) -> Ownership {
        self.edges
            .iter()
            .map(|e| e.ownership)
            .max()
            .unwrap_or(Ownership::Own)
    }
}

#[derive(Default)]
pub struct Registry {
    formats: HashMap<FormatId, FormatDescriptor>,
    edges: Vec<Edge>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add_format(&mut self, f: FormatDescriptor) {
        self.formats.insert(f.id, f);
    }
    pub fn add_edge(&mut self, e: Edge) {
        self.edges.push(e);
    }
    pub fn format(&self, id: FormatId) -> Option<&FormatDescriptor> {
        self.formats.get(&id)
    }
    /// Identify the best-matching format for `data` by magic bytes.
    pub fn detect(&self, data: &[u8]) -> Option<FormatId> {
        self.formats
            .values()
            .filter_map(|f| f.sniff(data).map(|c| (c, f.id)))
            .max_by_key(|(c, id)| (*c, Reverse(*id)))
            .map(|(_, id)| id)
    }

    /// Find the cheapest route, optionally forcing re-encoding (no stream copies).
    pub fn plan(&self, from: FormatId, to: FormatId, force_reencode: bool) -> Result<Route> {
        if from == to {
            return Ok(Route { edges: Vec::new() });
        }
        let mut best: HashMap<FormatId, Cost> = HashMap::new();
        let mut prev: HashMap<FormatId, usize> = HashMap::new();
        let mut heap = BinaryHeap::new();
        best.insert(from, Cost::ZERO);
        heap.push(Reverse((Cost::ZERO, from)));
        while let Some(Reverse((cost, node))) = heap.pop() {
            if node == to {
                break;
            }
            if best.get(&node).is_some_and(|b| *b < cost) {
                continue;
            }
            for (i, e) in self.edges.iter().enumerate() {
                if e.from != node || (force_reencode && e.kind == EdgeKind::StreamCopy) {
                    continue;
                }
                let next = cost.plus(e);
                if best.get(&e.to).is_none_or(|b| next < *b) {
                    best.insert(e.to, next.clone());
                    prev.insert(e.to, i);
                    heap.push(Reverse((next, e.to)));
                }
            }
        }
        if !best.contains_key(&to) {
            return Err(Error::new(
                ErrorCode::UnsupportedFormat,
                format!("no route from {} to {}", from.0, to.0),
            ));
        }
        let mut edges = Vec::new();
        let mut at = to;
        while at != from {
            let i = *prev
                .get(&at)
                .ok_or_else(|| Error::new(ErrorCode::InternalError, "broken route"))?;
            let e = self
                .edges
                .get(i)
                .ok_or_else(|| Error::new(ErrorCode::InternalError, "bad edge"))?;
            edges.push(e.clone());
            at = e.from;
        }
        edges.reverse();
        Ok(Route { edges })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: FormatId = FormatId("a");
    const B: FormatId = FormatId("b");
    const C: FormatId = FormatId("c");
    const D: FormatId = FormatId("d");

    fn edge(
        from: FormatId,
        to: FormatId,
        kind: EdgeKind,
        loss: u32,
        own: Ownership,
        time: u32,
    ) -> Edge {
        Edge {
            from,
            to,
            kind,
            loss,
            ownership: own,
            time,
            description: "step",
        }
    }

    #[test]
    fn prefers_lossless_over_faster() {
        let mut r = Registry::new();
        r.add_edge(edge(A, C, EdgeKind::Transcode, 5, Ownership::Own, 1));
        r.add_edge(edge(A, B, EdgeKind::Transcode, 0, Ownership::Own, 50));
        r.add_edge(edge(B, C, EdgeKind::Transcode, 0, Ownership::Own, 50));
        let route = r.plan(A, C, false).ok();
        assert_eq!(route.as_ref().map(|r| r.edges.len()), Some(2));
        assert_eq!(route.map(|r| r.total_loss()), Some(0));
    }

    #[test]
    fn prefers_own_over_partial_at_equal_loss() {
        let mut r = Registry::new();
        r.add_edge(edge(A, B, EdgeKind::Transcode, 0, Ownership::Partial, 1));
        r.add_edge(edge(A, C, EdgeKind::Transcode, 0, Ownership::Own, 9));
        r.add_edge(edge(C, B, EdgeKind::Transcode, 0, Ownership::Own, 9));
        let route = r.plan(A, B, false).ok();
        assert_eq!(route.map(|r| r.ownership()), Some(Ownership::Own));
    }

    #[test]
    fn stream_copy_chosen_unless_reencode_forced() {
        let mut r = Registry::new();
        r.add_edge(edge(A, B, EdgeKind::StreamCopy, 0, Ownership::Own, 1));
        r.add_edge(edge(A, B, EdgeKind::Transcode, 1, Ownership::Own, 100));
        let copy = r.plan(A, B, false).ok();
        assert_eq!(
            copy.and_then(|r| r.edges.first().map(|e| e.kind)),
            Some(EdgeKind::StreamCopy)
        );
        let forced = r.plan(A, B, true).ok();
        assert_eq!(
            forced.and_then(|r| r.edges.first().map(|e| e.kind)),
            Some(EdgeKind::Transcode)
        );
    }

    #[test]
    fn same_format_is_empty_route_and_missing_route_errors() {
        let mut r = Registry::new();
        r.add_edge(edge(A, B, EdgeKind::Transcode, 0, Ownership::Own, 1));
        assert_eq!(r.plan(A, A, false).map(|r| r.edges.len()).ok(), Some(0));
        let e = r.plan(A, D, false).err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::UnsupportedFormat));
    }

    #[test]
    fn explain_joins_steps() {
        let mut r = Registry::new();
        r.add_edge(edge(A, B, EdgeKind::Transcode, 0, Ownership::Own, 1));
        r.add_edge(edge(B, C, EdgeKind::Transcode, 0, Ownership::Own, 1));
        assert_eq!(
            r.plan(A, C, false).map(|r| r.explain()).ok().as_deref(),
            Some("step -> step")
        );
    }
}
