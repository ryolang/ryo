use super::super::{LoopFrame, MatchFrame};
use super::cascades::IfFrame;
use super::{Ownership, RegionStop, Walk, record_return_epilogue};
use ryo_core::diag::DiagSink;
use ryo_core::flow::{BlockId, Terminator};
use ryo_core::ownership::FunctionSidecar;
use ryo_core::spine::ChainSpine;

#[cfg(test)]
thread_local! {
    static STAGING_IO_RETAINED_AFTER_RESUME: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn staging_io_retained_after_resume() -> bool {
    STAGING_IO_RETAINED_AFTER_RESUME.with(std::cell::Cell::get)
}

#[derive(Clone, Default)]
pub(crate) struct FlowRegionSummary {
    pub(crate) exit: Option<BlockId>,
    pub(crate) falls_through: bool,
    pub(crate) continuations: Vec<BlockId>,
}

pub(crate) enum WalkAdvance {
    Done,
    Region {
        blocks: Vec<BlockId>,
        epoch: u32,
        phase: bool,
        staging: Option<Box<(DiagSink, FunctionSidecar)>>,
    },
    Open {
        entry: BlockId,
        stop: RegionStop,
        epoch: u32,
    },
    Flow {
        entry: BlockId,
        stop: RegionStop,
        epoch: u32,
    },
}

impl WalkAdvance {
    fn region(blocks: Vec<BlockId>, epoch: u32, phase: bool) -> Self {
        Self::Region {
            blocks,
            epoch,
            phase,
            staging: None,
        }
    }

    pub(crate) fn seen(blocks: Vec<BlockId>, epoch: u32) -> Self {
        Self::region(blocks, epoch, false)
    }

    pub(crate) fn phase(blocks: Vec<BlockId>, epoch: u32) -> Self {
        Self::region(blocks, epoch, true)
    }

    pub(crate) fn flow(entry: BlockId, stop: RegionStop, epoch: u32) -> Self {
        Self::Flow { entry, stop, epoch }
    }

    pub(crate) fn staging(
        blocks: Vec<BlockId>,
        epoch: u32,
        sink: DiagSink,
        sidecar: FunctionSidecar,
    ) -> Self {
        Self::Region {
            blocks,
            epoch,
            phase: true,
            staging: Some(Box::new((sink, sidecar))),
        }
    }
}

enum WalkIo<'a> {
    Real {
        sink: &'a mut DiagSink,
        sidecar: &'a mut FunctionSidecar,
    },
    Staging(DiagSink, Box<FunctionSidecar>),
}

impl WalkIo<'_> {
    fn parts(&mut self) -> (&mut DiagSink, &mut FunctionSidecar) {
        match self {
            Self::Real { sink, sidecar } => (sink, sidecar),
            Self::Staging(sink, sidecar) => (sink, sidecar),
        }
    }
}

#[derive(Clone, Copy)]
enum BlockMode {
    Open(RegionStop),
    Seen,
    Phase,
    Entered(Option<usize>),
}

enum WalkTask<'a> {
    Block {
        block: BlockId,
        epoch: u32,
        io: usize,
        mode: BlockMode,
    },
    Region {
        blocks: Vec<BlockId>,
        next: usize,
        epoch: u32,
        io: usize,
        phase: bool,
    },
    FlowRegion {
        pending: Vec<BlockId>,
        stop: RegionStop,
        epoch: u32,
        io: usize,
        result: usize,
    },
    Resume {
        target: Option<BlockId>,
        epoch: u32,
        io: usize,
        stop: RegionStop,
    },
    Match {
        frame: MatchFrame<'a>,
        io: usize,
    },
    If {
        frame: IfFrame,
        io: usize,
    },
    Loop {
        frame: LoopFrame,
        io: usize,
    },
}

impl<'a> Walk<'a> {
    /// Open-ended region walk. Control handlers return region actions to
    /// this loop, so no nested region starts another native walk.
    pub(crate) fn walk_region(
        &mut self,
        own: &mut Ownership,
        sink: &mut DiagSink,
        sidecar: &mut FunctionSidecar,
        entry: BlockId,
        stop: RegionStop,
        epoch: u32,
    ) -> bool {
        #[cfg(test)]
        STAGING_IO_RETAINED_AFTER_RESUME.with(|retained| retained.set(false));
        let mut reached_stop = false;
        let mut stack = ChainSpine::new();
        let mut ios = vec![Some(WalkIo::Real { sink, sidecar })];
        let mut region_results: Vec<FlowRegionSummary> = Vec::new();
        stack.push(WalkTask::Block {
            block: entry,
            epoch,
            io: 0,
            mode: BlockMode::Open(stop),
        });
        while let Some(task) = stack.pop() {
            match task {
                WalkTask::Block {
                    block,
                    epoch,
                    io,
                    mode,
                } => {
                    let entered = match mode {
                        BlockMode::Open(stop) => {
                            if stop.excludes(block) {
                                reached_stop = true;
                                continue;
                            }
                            self.enter_seen(block, epoch)
                        }
                        BlockMode::Seen => self.enter_seen(block, epoch),
                        BlockMode::Phase => self.enter_epoch(block, epoch),
                        BlockMode::Entered(_) => true,
                    };
                    if !entered {
                        continue;
                    }
                    self.visit_block(own, &mut ios, &mut stack, block, epoch, io, mode);
                }
                WalkTask::Region {
                    blocks,
                    next,
                    epoch,
                    io,
                    phase,
                } => {
                    if next == blocks.len() {
                        continue;
                    }
                    let block = blocks[next];
                    stack.push(WalkTask::Region {
                        blocks,
                        next: next + 1,
                        epoch,
                        io,
                        phase,
                    });
                    stack.push(WalkTask::Block {
                        block,
                        epoch,
                        io,
                        mode: if phase {
                            BlockMode::Phase
                        } else {
                            BlockMode::Seen
                        },
                    });
                }
                WalkTask::FlowRegion {
                    mut pending,
                    stop,
                    epoch,
                    io,
                    result,
                } => {
                    pending.extend(std::mem::take(&mut region_results[result].continuations));
                    while let Some(block) = pending.pop() {
                        if stop.excludes(block) || self.epoch[block.0 as usize] == epoch {
                            continue;
                        }
                        if !self.enter_seen(block, epoch) {
                            continue;
                        }
                        let summary = &mut region_results[result];
                        if summary.exit.is_none()
                            && matches!(
                                self.flow.term(block),
                                Terminator::Return { .. }
                                    | Terminator::ReturnVoid
                                    | Terminator::Break { .. }
                                    | Terminator::Continue { .. }
                            )
                        {
                            summary.exit = Some(block);
                        }
                        if let RegionStop::At(join) = stop
                            && matches!(
                                self.flow.term(block),
                                Terminator::Goto { target } if *target == join
                            )
                        {
                            summary.falls_through = true;
                        }
                        if !stop.freezes(block) {
                            for successor in
                                super::region_successors(self.flow, block).into_iter().rev()
                            {
                                pending.push(successor);
                            }
                        }
                        stack.push(WalkTask::FlowRegion {
                            pending,
                            stop,
                            epoch,
                            io,
                            result,
                        });
                        stack.push(WalkTask::Block {
                            block,
                            epoch,
                            io,
                            mode: BlockMode::Entered(Some(result)),
                        });
                        break;
                    }
                }
                WalkTask::Resume {
                    target,
                    epoch,
                    io,
                    stop,
                } => {
                    if let Some(target) = target {
                        stack.push(WalkTask::Block {
                            block: target,
                            epoch,
                            io,
                            mode: BlockMode::Open(stop),
                        });
                    }
                }
                WalkTask::Match { mut frame, io } => {
                    if let Some(result) = frame.region_id.take() {
                        frame.region_summary = Some(std::mem::take(&mut region_results[result]));
                    }
                    let action = {
                        let (sink, sidecar) = ios[io].as_mut().expect("match IO").parts();
                        frame.advance(self, own, sink, sidecar)
                    };
                    if !matches!(action, WalkAdvance::Done) {
                        if matches!(action, WalkAdvance::Flow { .. }) {
                            frame.region_id = Some(region_results.len());
                            region_results.push(FlowRegionSummary::default());
                        }
                        stack.push(WalkTask::Match { frame, io });
                        Self::schedule_action(
                            &mut stack,
                            &mut ios,
                            io,
                            action,
                            &mut region_results,
                        );
                    } else if let Some(parent) = frame.parent_region {
                        let summary = &mut region_results[parent];
                        if summary.exit.is_none() {
                            summary.exit = frame.region_exit;
                        }
                        if let Some(join) = frame.join {
                            summary.continuations.push(join);
                        }
                    }
                }
                WalkTask::If { mut frame, io } => {
                    let action = {
                        let (sink, sidecar) = ios[io].as_mut().expect("if IO").parts();
                        frame.advance(self, own, sink, sidecar)
                    };
                    if !matches!(action, WalkAdvance::Done) {
                        stack.push(WalkTask::If { frame, io });
                        Self::schedule_action(
                            &mut stack,
                            &mut ios,
                            io,
                            action,
                            &mut region_results,
                        );
                    } else if let Some(parent) = frame.parent_region {
                        let summary = &mut region_results[parent];
                        if summary.exit.is_none() {
                            summary.exit = frame.region_exit;
                        }
                        summary.continuations.push(frame.join);
                    }
                }
                WalkTask::Loop { mut frame, io } => {
                    if let Some(staging) = frame.pass_io.take() {
                        ios[staging] = None;
                    }
                    #[cfg(test)]
                    if ios.iter().any(|io| matches!(io, Some(WalkIo::Staging(..)))) {
                        STAGING_IO_RETAINED_AFTER_RESUME.with(|retained| retained.set(true));
                    }
                    let action = {
                        let (sink, sidecar) = ios[io].as_mut().expect("loop IO").parts();
                        frame.advance(self, own, sink, sidecar)
                    };
                    if !matches!(action, WalkAdvance::Done) {
                        if matches!(
                            action,
                            WalkAdvance::Region {
                                staging: Some(_),
                                ..
                            }
                        ) {
                            frame.pass_io = Some(ios.len());
                        }
                        stack.push(WalkTask::Loop { frame, io });
                        Self::schedule_action(
                            &mut stack,
                            &mut ios,
                            io,
                            action,
                            &mut region_results,
                        );
                    } else if let Some(parent) = frame.parent_region {
                        region_results[parent].continuations.push(frame.exit);
                    }
                }
            }
        }
        reached_stop
    }

    fn schedule_action(
        stack: &mut ChainSpine<WalkTask<'a>>,
        ios: &mut Vec<Option<WalkIo<'_>>>,
        current_io: usize,
        action: WalkAdvance,
        region_results: &mut [FlowRegionSummary],
    ) {
        match action {
            WalkAdvance::Done => {}
            WalkAdvance::Region {
                blocks,
                epoch,
                phase,
                staging,
            } => {
                let io = if let Some((sink, sidecar)) = staging.map(|staging| *staging) {
                    ios.push(Some(WalkIo::Staging(sink, Box::new(sidecar))));
                    ios.len() - 1
                } else {
                    current_io
                };
                stack.push(WalkTask::Region {
                    blocks,
                    next: 0,
                    epoch,
                    io,
                    phase,
                });
            }
            WalkAdvance::Open { entry, stop, epoch } => stack.push(WalkTask::Block {
                block: entry,
                epoch,
                io: current_io,
                mode: BlockMode::Open(stop),
            }),
            WalkAdvance::Flow { entry, stop, epoch } => {
                stack.push(WalkTask::FlowRegion {
                    pending: vec![entry],
                    stop,
                    epoch,
                    io: current_io,
                    result: region_results.len() - 1,
                });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn visit_block(
        &mut self,
        own: &mut Ownership,
        ios: &mut [Option<WalkIo<'_>>],
        stack: &mut ChainSpine<WalkTask<'a>>,
        b: BlockId,
        epoch: u32,
        io: usize,
        mode: BlockMode,
    ) {
        let dead = self.flow.blocks[b.0 as usize].unreachable;
        {
            let (sink, sidecar) = ios[io].as_mut().expect("block IO").parts();
            for step in self.flow.block_steps(b) {
                self.process_step(own, sink, sidecar, *step, dead);
            }
        }
        let follow = match mode {
            BlockMode::Open(stop) if !stop.freezes(b) => Some(stop),
            _ => None,
        };
        match *self.flow.term(b) {
            Terminator::Goto { target } => {
                if let Some(stop) = follow {
                    stack.push(WalkTask::Block {
                        block: target,
                        epoch,
                        io,
                        mode: BlockMode::Open(stop),
                    });
                }
            }
            Terminator::Branch { then_, else_, .. } => {
                if let Some(if_inst) = self.flow.branch_if(b) {
                    let (mut frame, join) = {
                        let (sink, sidecar) = ios[io].as_mut().expect("if IO").parts();
                        IfFrame::new(self, own, sink, sidecar, b, if_inst, epoch)
                    };
                    if let Some(stop) = follow {
                        stack.push(WalkTask::Resume {
                            target: Some(join),
                            epoch,
                            io,
                            stop,
                        });
                    }
                    frame.parent_region = match mode {
                        BlockMode::Entered(result) => result,
                        _ => None,
                    };
                    stack.push(WalkTask::If { frame, io });
                } else if let Some(stop) = follow {
                    stack.push(WalkTask::Block {
                        block: else_,
                        epoch,
                        io,
                        mode: BlockMode::Open(stop),
                    });
                    stack.push(WalkTask::Block {
                        block: then_,
                        epoch,
                        io,
                        mode: BlockMode::Open(stop),
                    });
                }
            }
            Terminator::Switch { match_inst, .. } => {
                let (frame, join) = {
                    let (sink, sidecar) = ios[io].as_mut().expect("match IO").parts();
                    MatchFrame::new(self, own, sink, sidecar, b, match_inst, epoch)
                };
                if let Some(stop) = follow {
                    stack.push(WalkTask::Resume {
                        target: join,
                        epoch,
                        io,
                        stop,
                    });
                }
                if let Some(mut frame) = frame {
                    frame.parent_region = match mode {
                        BlockMode::Entered(result) => result,
                        _ => None,
                    };
                    stack.push(WalkTask::Match { frame, io });
                }
            }
            Terminator::While { .. } | Terminator::For { .. } => {
                let (mut frame, exit) = LoopFrame::new(self, own, b, epoch);
                if let Some(stop) = follow {
                    stack.push(WalkTask::Resume {
                        target: Some(exit),
                        epoch,
                        io,
                        stop,
                    });
                }
                frame.parent_region = match mode {
                    BlockMode::Entered(result) => result,
                    _ => None,
                };
                stack.push(WalkTask::Loop { frame, io });
            }
            Terminator::Return { .. } => {
                if let Some(payload) = self.flow.term_payload(b) {
                    let sink = match ios[io].as_mut().expect("return IO") {
                        WalkIo::Real { sink, .. } => sink,
                        WalkIo::Staging(sink, _) => sink,
                    };
                    self.analyze_return(own, sink, payload);
                    record_return_epilogue(self.tir, self.flow, own, payload);
                }
            }
            Terminator::ReturnVoid => {
                if let Some(payload) = self.flow.term_payload(b) {
                    record_return_epilogue(self.tir, self.flow, own, payload);
                }
            }
            Terminator::Backedge { .. }
            | Terminator::Break { .. }
            | Terminator::Continue { .. }
            | Terminator::Unreachable
            | Terminator::WhileTest { .. }
            | Terminator::ForTest { .. } => {}
            Terminator::Call { .. } => {
                let payload = self
                    .flow
                    .term_payload(b)
                    .expect("diverging calls carry their instruction");
                let sink = match ios[io].as_mut().expect("call IO") {
                    WalkIo::Real { sink, .. } => sink,
                    WalkIo::Staging(sink, _) => sink,
                };
                self.analyze_call(own, sink, payload);
            }
        }
    }
}
