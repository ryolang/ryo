//! Resolve cleanup storage before machine-code emission.

use super::{
    DropPosition, Flow, PlaceId, WriteKind,
    fire_points::{FirePoints, FireVerdict},
};
use crate::ownership::FunctionSidecar;
use crate::tir::{Tir, TirRef, TirTag};
use crate::types::{InternPool, TypeId};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseStorage {
    /// Read the path's current contents of this exact declaring binding.
    Binding(PlaceId),
    /// Destroy the produced value, which a later write displaced from its home.
    Value(TirRef),
}

impl ReleaseStorage {
    /// A static producer owns no allocation. A mutable home can still own one.
    pub fn is_static_noop(self, tir: &Tir) -> bool {
        matches!(self, Self::Value(target) if !target.is_param()
            && matches!(tir.inst(target).tag, TirTag::StrConst | TirTag::BytesConst))
    }
}

/// An unbound payload field's concrete storage inside its scrutinee home.
#[derive(Clone, Copy, Debug)]
pub struct PayloadField {
    pub offset: u32,
    pub ty: TypeId,
}

#[derive(Clone, Debug)]
pub struct PayloadRelease {
    pub match_inst: TirRef,
    pub fields: Vec<PayloadField>,
}

/// Storage selection is independent of register caches and active source names.
pub struct CleanupPlan {
    pub releases: Vec<ReleaseStorage>,
    pub payloads: Vec<Option<PayloadRelease>>,
}

impl CleanupPlan {
    pub fn resolve(
        tir: &Tir,
        flow: &Flow,
        cleanup: &FunctionSidecar,
        pool: &InternPool,
    ) -> Result<Self, String> {
        // Callers enforce artifact association at their phase boundary. This
        // resolver also serves validators that deliberately corrupt raw schedules.
        let mut homes = vec![None; tir.instructions.len()];
        let mut last = vec![None; flow.places.len()];
        for step in &flow.steps {
            let Some(write) = step.facts.write else {
                continue;
            };
            let value = match (tir.inst(step.inst).tag, write.kind) {
                (TirTag::VarDecl, WriteKind::Init) => {
                    homes[step.inst.index()] = Some(write.place);
                    tir.var_decl_view(step.inst).initializer
                }
                (TirTag::Assign, WriteKind::Replace) => tir.assign_view(step.inst).value,
                _ => continue,
            };
            homes[value.index()] = Some(write.place);
            last[write.place.0 as usize] = Some(value);
        }
        for (index, place) in flow.places.iter().enumerate() {
            if let super::Place::Token { inst, .. } = place {
                homes[inst.index()] = Some(PlaceId(index as u32));
                last[index] = Some(*inst);
            }
        }
        let targets: HashSet<_> = cleanup.drops.iter().map(|drop| drop.target).collect();
        let points = FirePoints::new(flow);
        let mut releases = Vec::with_capacity(cleanup.drops.len());
        let mut payloads = Vec::with_capacity(cleanup.drops.len());
        let mut payload_arms = std::collections::HashMap::new();
        for term in &flow.terms {
            if let super::Terminator::Switch {
                match_inst,
                scrutinee,
                arms,
                ..
            } = *term
            {
                for (arm, index) in arms.as_range().enumerate() {
                    payload_arms.insert(
                        (super::BlockId(flow.extra[index]), scrutinee),
                        (match_inst, arm),
                    );
                }
            }
        }
        for drop in &cleanup.drops {
            let payload = if let DropPosition::BlockStart { block } = drop.position {
                payload_arms
                    .get(&(block, drop.target))
                    .map(|&(match_inst, arm_index)| {
                        let view = tir.match_view(match_inst);
                        let arm = view
                            .arms()
                            .nth(arm_index)
                            .expect("Flow arm matches TIR payload");
                        let ty = tir.inst(view.scrutinee).ty;
                        let fields = match arm.discriminant {
                            crate::tir::Discriminant::Variant(index) => pool
                                .enum_view(ty)
                                .variants()
                                .nth(index as usize)
                                .expect("typed variant")
                                .fields
                                .to_vec(),
                            _ => pool.struct_view(ty).fields.to_vec(),
                        };
                        PayloadRelease {
                            match_inst,
                            fields: fields
                                .into_iter()
                                .filter(|field| {
                                    pool.needs_drop(field.ty)
                                        && !arm
                                            .bindings()
                                            .any(|binding| binding.field_index == field.idx)
                                })
                                .map(|field| PayloadField {
                                    offset: field.offset,
                                    ty: field.ty,
                                })
                                .collect(),
                        }
                    })
            } else {
                None
            };
            payloads.push(payload);
            if matches!(points.resolve(drop.position), FireVerdict::Unresolved) {
                return Err(format!("unresolved cleanup position {:?}", drop.position));
            }
            let home = if let Some(index) = drop.target.as_param_index() {
                flow.places
                    .iter()
                    .position(|p| matches!(p, super::Place::Param { index: n, .. } if *n == index))
                    .map(|index| PlaceId(index as u32))
            } else {
                homes.get(drop.target.index()).copied().flatten()
            };
            let binding = home.filter(|place| {
                matches!(drop.position, DropPosition::BlockStart { .. })
                    || drop.target.is_param()
                    || last[place.0 as usize]
                        .is_some_and(|value| value == drop.target || !targets.contains(&value))
            });
            releases.push(
                binding
                    .map(ReleaseStorage::Binding)
                    .unwrap_or(ReleaseStorage::Value(drop.target)),
            );
        }
        for release in &cleanup.promotion_frees {
            if matches!(points.resolve(release.position), FireVerdict::Unresolved) {
                return Err(format!(
                    "unresolved promotion position {:?}",
                    release.position
                ));
            }
        }
        Ok(Self { releases, payloads })
    }
}
