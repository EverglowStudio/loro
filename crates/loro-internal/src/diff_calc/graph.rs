use super::{DiffCalcVersionInfo, DiffCalculatorTrait, DiffMode};
use crate::{
    container::{
        graph::{GraphChange, GraphDiff, GraphOp},
        idx::ContainerIdx,
    },
    event::InternalDiff,
    op::RichOp,
    span::HasLamport,
    OpLog,
};
use loro_common::{ContainerID, ID};
use std::collections::BTreeMap;
#[derive(Debug)]
pub(crate) struct GraphDiffCalculator {
    ops: BTreeMap<ID, (u32, GraphOp)>,
}
impl GraphDiffCalculator {
    pub fn new() -> Self {
        Self {
            ops: BTreeMap::new(),
        }
    }
}
impl DiffCalculatorTrait for GraphDiffCalculator {
    fn start_tracking(&mut self, _oplog: &OpLog, _vv: &crate::VersionVector, _mode: DiffMode) {}
    fn apply_change(&mut self, _oplog: &OpLog, op: RichOp, _vv: Option<&crate::VersionVector>) {
        self.ops.insert(
            op.id(),
            (
                op.lamport(),
                (**op.op().content.as_graph().unwrap()).clone(),
            ),
        );
    }
    fn finish_this_round(&mut self) {}
    fn calculate_diff(
        &mut self,
        _idx: ContainerIdx,
        _oplog: &OpLog,
        info: DiffCalcVersionInfo,
        mut on_new: impl FnMut(&ContainerID),
    ) -> (InternalDiff, DiffMode) {
        let (removed, added) = info.from_vv.diff_iter(info.to_vv);
        let mut back = Vec::new();
        let mut forward = Vec::new();
        for span in removed {
            for (id, (lp, op)) in self.ops.range(span.norm_id_start()..span.norm_id_end()) {
                back.push((
                    *lp,
                    GraphChange {
                        id: *id,
                        op: op.clone(),
                        forward: false,
                    },
                ));
            }
        }
        for span in added {
            for (id, (lp, op)) in self.ops.range(span.norm_id_start()..span.norm_id_end()) {
                if let Some(c) = op.created_meta() {
                    on_new(&c);
                }
                forward.push((
                    *lp,
                    GraphChange {
                        id: *id,
                        op: op.clone(),
                        forward: true,
                    },
                ));
            }
        }
        back.sort_by_key(|(lp, c)| std::cmp::Reverse((*lp, c.id)));
        forward.sort_by_key(|(lp, c)| (*lp, c.id));
        let ops = back.into_iter().chain(forward).map(|(_, c)| c).collect();
        (
            InternalDiff::Graph(GraphDiff {
                ops,
                ..Default::default()
            }),
            DiffMode::Linear,
        )
    }
}
