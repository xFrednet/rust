use rustc_index::IndexVec;
use rustc_middle::mir::tcx::RvalueInitializationState;
use rustc_middle::mir::*;
use rustc_middle::ty::{self, TyCtxt};
use smallvec::{smallvec, SmallVec};

use std::mem;

use super::abs_domain::Lift;
use super::IllegalMoveOriginKind::*;
use super::{Init, InitIndex, InitKind, InitLocation, LookupResult, MoveError};
use super::{
    LocationMap, MoveData, MoveOut, MoveOutIndex, MovePath, MovePathIndex, MovePathLookup,
};

struct MoveDataBuilder<'a, 'tcx> {
    body: &'a Body<'tcx>,
    tcx: TyCtxt<'tcx>,
    param_env: ty::ParamEnv<'tcx>,
    data: MoveData<'tcx>,
}

impl<'a, 'tcx> MoveDataBuilder<'a, 'tcx> {
    fn new(body: &'a Body<'tcx>, tcx: TyCtxt<'tcx>, param_env: ty::ParamEnv<'tcx>) -> Self {
        let mut move_paths = IndexVec::new();
        let mut path_map = IndexVec::new();
        let mut init_path_map = IndexVec::new();

        MoveDataBuilder {
            body,
            tcx,
            param_env,
            data: MoveData {
                moves: IndexVec::new(),
                loc_map: LocationMap::new(body),
                rev_lookup: MovePathLookup {
                    locals: body
                        .local_decls
                        .iter_enumerated()
                        .map(|(i, l)| {
                            if l.is_deref_temp() {
                                None
                            } else {
                                Some(Self::new_move_path(
                                    &mut move_paths,
                                    &mut path_map,
                                    &mut init_path_map,
                                    None,
                                    Place::from(i),
                                ))
                            }
                        })
                        .collect(),
                    projections: Default::default(),
                    un_derefer: Default::default(),
                },
                move_paths,
                path_map,
                inits: IndexVec::new(),
                init_loc_map: LocationMap::new(body),
                init_path_map,
            },
        }
    }

    fn new_move_path(
        move_paths: &mut IndexVec<MovePathIndex, MovePath<'tcx>>,
        path_map: &mut IndexVec<MovePathIndex, SmallVec<[MoveOutIndex; 4]>>,
        init_path_map: &mut IndexVec<MovePathIndex, SmallVec<[InitIndex; 4]>>,
        parent: Option<MovePathIndex>,
        place: Place<'tcx>,
    ) -> MovePathIndex {
        let move_path =
            move_paths.push(MovePath { next_sibling: None, first_child: None, parent, place });

        if let Some(parent) = parent {
            let next_sibling = mem::replace(&mut move_paths[parent].first_child, Some(move_path));
            move_paths[move_path].next_sibling = next_sibling;
        }

        let path_map_ent = path_map.push(smallvec![]);
        assert_eq!(path_map_ent, move_path);

        let init_path_map_ent = init_path_map.push(smallvec![]);
        assert_eq!(init_path_map_ent, move_path);

        move_path
    }
}

impl<'b, 'a, 'tcx> Gatherer<'b, 'a, 'tcx> {
    /// This creates a MovePath for a given place, returning an `MovePathError`
    /// if that place can't be moved from.
    ///
    /// NOTE: places behind references *do not* get a move path, which is
    /// problematic for borrowck.
    ///
    /// Maybe we should have separate "borrowck" and "moveck" modes.
    fn move_path_for(&mut self, place: Place<'tcx>) -> Result<MovePathIndex, MoveError<'tcx>> {
        let data = &mut self.builder.data;

        debug!("lookup({:?})", place);
        let Some(mut base) = data.rev_lookup.find_local(place.local) else {
            return Err(MoveError::UntrackedLocal);
        };

        // The move path index of the first union that we find. Once this is
        // some we stop creating child move paths, since moves from unions
        // move the whole thing.
        // We continue looking for other move errors though so that moving
        // from `*(u.f: &_)` isn't allowed.
        let mut union_path = None;

        for (place_ref, elem) in data.rev_lookup.un_derefer.iter_projections(place.as_ref()) {
            let body = self.builder.body;
            let tcx = self.builder.tcx;
            let place_ty = place_ref.ty(body, tcx).ty;
            match elem {
                ProjectionElem::Deref => match place_ty.kind() {
                    ty::Ref(..) | ty::RawPtr(..) => {
                        return Err(MoveError::cannot_move_out_of(
                            self.loc,
                            BorrowedContent {
                                target_place: place_ref.project_deeper(&[elem], tcx),
                            },
                        ));
                    }
                    ty::Adt(adt, _) => {
                        if !adt.is_box() {
                            bug!("Adt should be a box type when Place is deref");
                        }
                    }
                    ty::Bool
                    | ty::Char
                    | ty::Int(_)
                    | ty::Uint(_)
                    | ty::Float(_)
                    | ty::Foreign(_)
                    | ty::Str
                    | ty::Array(_, _)
                    | ty::Slice(_)
                    | ty::FnDef(_, _)
                    | ty::FnPtr(_)
                    | ty::Dynamic(_, _, _)
                    | ty::Closure(_, _)
                    | ty::Coroutine(_, _, _)
                    | ty::CoroutineWitness(..)
                    | ty::Never
                    | ty::Tuple(_)
                    | ty::Alias(_, _)
                    | ty::Param(_)
                    | ty::Bound(_, _)
                    | ty::Infer(_)
                    | ty::Error(_)
                    | ty::Placeholder(_) => {
                        bug!("When Place is Deref it's type shouldn't be {place_ty:#?}")
                    }
                },
                ProjectionElem::Field(_, _) => match place_ty.kind() {
                    ty::Adt(adt, _) => {
                        if adt.has_dtor(tcx) {
                            return Err(MoveError::cannot_move_out_of(
                                self.loc,
                                InteriorOfTypeWithDestructor { container_ty: place_ty },
                            ));
                        }
                        if adt.is_union() {
                            union_path.get_or_insert(base);
                        }
                    }
                    ty::Closure(_, _) | ty::Coroutine(_, _, _) | ty::Tuple(_) => (),
                    ty::Bool
                    | ty::Char
                    | ty::Int(_)
                    | ty::Uint(_)
                    | ty::Float(_)
                    | ty::Foreign(_)
                    | ty::Str
                    | ty::Array(_, _)
                    | ty::Slice(_)
                    | ty::RawPtr(_)
                    | ty::Ref(_, _, _)
                    | ty::FnDef(_, _)
                    | ty::FnPtr(_)
                    | ty::Dynamic(_, _, _)
                    | ty::CoroutineWitness(..)
                    | ty::Never
                    | ty::Alias(_, _)
                    | ty::Param(_)
                    | ty::Bound(_, _)
                    | ty::Infer(_)
                    | ty::Error(_)
                    | ty::Placeholder(_) => bug!(
                        "When Place contains ProjectionElem::Field it's type shouldn't be {place_ty:#?}"
                    ),
                },
                ProjectionElem::ConstantIndex { .. } | ProjectionElem::Subslice { .. } => {
                    match place_ty.kind() {
                        ty::Slice(_) => {
                            return Err(MoveError::cannot_move_out_of(
                                self.loc,
                                InteriorOfSliceOrArray {
                                    ty: place_ty,
                                    is_index: matches!(elem, ProjectionElem::Index(..)),
                                },
                            ));
                        }
                        ty::Array(_, _) => (),
                        _ => bug!("Unexpected type {:#?}", place_ty.is_array()),
                    }
                }
                ProjectionElem::Index(_) => match place_ty.kind() {
                    ty::Array(..) => {
                        return Err(MoveError::cannot_move_out_of(
                            self.loc,
                            InteriorOfSliceOrArray { ty: place_ty, is_index: true },
                        ));
                    }
                    ty::Slice(_) => {
                        return Err(MoveError::cannot_move_out_of(
                            self.loc,
                            InteriorOfSliceOrArray {
                                ty: place_ty,
                                is_index: matches!(elem, ProjectionElem::Index(..)),
                            },
                        ));
                    }
                    _ => bug!("Unexpected type {place_ty:#?}"),
                },
                // `OpaqueCast`:Only transmutes the type, so no moves there.
                // `Downcast`  :Only changes information about a `Place` without moving.
                // `Subtype`   :Only transmutes the type, so moves.
                // So it's safe to skip these.
                ProjectionElem::OpaqueCast(_)
                | ProjectionElem::Subtype(_)
                | ProjectionElem::Downcast(_, _) => (),
            }
            if union_path.is_none() {
                // inlined from add_move_path because of a borrowck conflict with the iterator
                base =
                    *data.rev_lookup.projections.entry((base, elem.lift())).or_insert_with(|| {
                        MoveDataBuilder::new_move_path(
                            &mut data.move_paths,
                            &mut data.path_map,
                            &mut data.init_path_map,
                            Some(base),
                            place_ref.project_deeper(&[elem], tcx),
                        )
                    })
            }
        }

        if let Some(base) = union_path {
            // Move out of union - always move the entire union.
            Err(MoveError::UnionMove { path: base })
        } else {
            Ok(base)
        }
    }

    fn add_move_path(
        &mut self,
        base: MovePathIndex,
        elem: PlaceElem<'tcx>,
        mk_place: impl FnOnce(TyCtxt<'tcx>) -> Place<'tcx>,
    ) -> MovePathIndex {
        let MoveDataBuilder {
            data: MoveData { rev_lookup, move_paths, path_map, init_path_map, .. },
            tcx,
            ..
        } = self.builder;
        *rev_lookup.projections.entry((base, elem.lift())).or_insert_with(move || {
            MoveDataBuilder::new_move_path(
                move_paths,
                path_map,
                init_path_map,
                Some(base),
                mk_place(*tcx),
            )
        })
    }

    fn create_move_path(&mut self, place: Place<'tcx>) {
        // This is an non-moving access (such as an overwrite or
        // drop), so this not being a valid move path is OK.
        let _ = self.move_path_for(place);
    }
}

impl<'a, 'tcx> MoveDataBuilder<'a, 'tcx> {
    fn finalize(self) -> MoveData<'tcx> {
        debug!("{}", {
            debug!("moves for {:?}:", self.body.span);
            for (j, mo) in self.data.moves.iter_enumerated() {
                debug!("    {:?} = {:?}", j, mo);
            }
            debug!("move paths for {:?}:", self.body.span);
            for (j, path) in self.data.move_paths.iter_enumerated() {
                debug!("    {:?} = {:?}", j, path);
            }
            "done dumping moves"
        });

        self.data
    }
}

pub(super) fn gather_moves<'tcx>(
    body: &Body<'tcx>,
    tcx: TyCtxt<'tcx>,
    param_env: ty::ParamEnv<'tcx>,
) -> MoveData<'tcx> {
    let mut builder = MoveDataBuilder::new(body, tcx, param_env);

    builder.gather_args();

    for (bb, block) in body.basic_blocks.iter_enumerated() {
        for (i, stmt) in block.statements.iter().enumerate() {
            let source = Location { block: bb, statement_index: i };
            builder.gather_statement(source, stmt);
        }

        let terminator_loc = Location { block: bb, statement_index: block.statements.len() };
        builder.gather_terminator(terminator_loc, block.terminator());
    }

    builder.finalize()
}

impl<'a, 'tcx> MoveDataBuilder<'a, 'tcx> {
    fn gather_args(&mut self) {
        for arg in self.body.args_iter() {
            if let Some(path) = self.data.rev_lookup.find_local(arg) {
                let init = self.data.inits.push(Init {
                    path,
                    kind: InitKind::Deep,
                    location: InitLocation::Argument(arg),
                });

                debug!("gather_args: adding init {:?} of {:?} for argument {:?}", init, path, arg);

                self.data.init_path_map[path].push(init);
            }
        }
    }

    fn gather_statement(&mut self, loc: Location, stmt: &Statement<'tcx>) {
        debug!("gather_statement({:?}, {:?})", loc, stmt);
        (Gatherer { builder: self, loc }).gather_statement(stmt);
    }

    fn gather_terminator(&mut self, loc: Location, term: &Terminator<'tcx>) {
        debug!("gather_terminator({:?}, {:?})", loc, term);
        (Gatherer { builder: self, loc }).gather_terminator(term);
    }
}

struct Gatherer<'b, 'a, 'tcx> {
    builder: &'b mut MoveDataBuilder<'a, 'tcx>,
    loc: Location,
}

impl<'b, 'a, 'tcx> Gatherer<'b, 'a, 'tcx> {
    fn gather_statement(&mut self, stmt: &Statement<'tcx>) {
        match &stmt.kind {
            StatementKind::Assign(box (place, Rvalue::CopyForDeref(reffed))) => {
                let local = place.as_local().unwrap();
                assert!(self.builder.body.local_decls[local].is_deref_temp());

                let rev_lookup = &mut self.builder.data.rev_lookup;

                rev_lookup.un_derefer.insert(local, reffed.as_ref());
                let base_local = rev_lookup.un_derefer.deref_chain(local).first().unwrap().local;
                rev_lookup.locals[local] = rev_lookup.locals[base_local];
            }
            StatementKind::Assign(box (place, rval)) => {
                self.create_move_path(*place);
                if let RvalueInitializationState::Shallow = rval.initialization_state() {
                    // Box starts out uninitialized - need to create a separate
                    // move-path for the interior so it will be separate from
                    // the exterior.
                    self.create_move_path(self.builder.tcx.mk_place_deref(*place));
                    self.gather_init(place.as_ref(), InitKind::Shallow);
                } else {
                    self.gather_init(place.as_ref(), InitKind::Deep);
                }
                self.gather_rvalue(rval);
            }
            StatementKind::FakeRead(box (_, place)) => {
                self.create_move_path(*place);
            }
            StatementKind::StorageLive(_) => {}
            StatementKind::StorageDead(local) => {
                // DerefTemp locals (results of CopyForDeref) don't actually move anything.
                if !self.builder.body.local_decls[*local].is_deref_temp() {
                    self.gather_move(Place::from(*local));
                }
            }
            StatementKind::SetDiscriminant { .. } | StatementKind::Deinit(..) => {
                span_bug!(
                    stmt.source_info.span,
                    "SetDiscriminant/Deinit should not exist during borrowck"
                );
            }
            StatementKind::Retag { .. }
            | StatementKind::AscribeUserType(..)
            | StatementKind::PlaceMention(..)
            | StatementKind::Coverage(..)
            | StatementKind::Intrinsic(..)
            | StatementKind::ConstEvalCounter
            | StatementKind::Nop => {}
        }
    }

    fn gather_rvalue(&mut self, rvalue: &Rvalue<'tcx>) {
        match *rvalue {
            Rvalue::ThreadLocalRef(_) => {} // not-a-move
            Rvalue::Use(ref operand)
            | Rvalue::Repeat(ref operand, _)
            | Rvalue::Cast(_, ref operand, _)
            | Rvalue::ShallowInitBox(ref operand, _)
            | Rvalue::UnaryOp(_, ref operand) => self.gather_operand(operand),
            Rvalue::BinaryOp(ref _binop, box (ref lhs, ref rhs))
            | Rvalue::CheckedBinaryOp(ref _binop, box (ref lhs, ref rhs)) => {
                self.gather_operand(lhs);
                self.gather_operand(rhs);
            }
            Rvalue::Aggregate(ref _kind, ref operands) => {
                for operand in operands {
                    self.gather_operand(operand);
                }
            }
            Rvalue::CopyForDeref(..) => unreachable!(),
            Rvalue::Ref(..)
            | Rvalue::AddressOf(..)
            | Rvalue::Discriminant(..)
            | Rvalue::Len(..)
            | Rvalue::NullaryOp(NullOp::SizeOf | NullOp::AlignOf | NullOp::OffsetOf(..), _) => {}
        }
    }

    fn gather_terminator(&mut self, term: &Terminator<'tcx>) {
        match term.kind {
            TerminatorKind::Goto { target: _ }
            | TerminatorKind::FalseEdge { .. }
            | TerminatorKind::FalseUnwind { .. }
            // In some sense returning moves the return place into the current
            // call's destination, however, since there are no statements after
            // this that could possibly access the return place, this doesn't
            // need recording.
            | TerminatorKind::Return
            | TerminatorKind::UnwindResume
            | TerminatorKind::UnwindTerminate(_)
            | TerminatorKind::CoroutineDrop
            | TerminatorKind::Unreachable
            | TerminatorKind::Drop { .. } => {}

            TerminatorKind::Assert { ref cond, .. } => {
                self.gather_operand(cond);
            }

            TerminatorKind::SwitchInt { ref discr, .. } => {
                self.gather_operand(discr);
            }

            TerminatorKind::Yield { ref value, resume_arg: place, .. } => {
                self.gather_operand(value);
                self.create_move_path(place);
                self.gather_init(place.as_ref(), InitKind::Deep);
            }
            TerminatorKind::Call {
                ref func,
                ref args,
                destination,
                target,
                unwind: _,
                call_source: _,
                fn_span: _,
            } => {
                self.gather_operand(func);
                for arg in args {
                    self.gather_operand(arg);
                }
                if let Some(_bb) = target {
                    self.create_move_path(destination);
                    self.gather_init(destination.as_ref(), InitKind::NonPanicPathOnly);
                }
            }
            TerminatorKind::InlineAsm {
                template: _,
                ref operands,
                options: _,
                line_spans: _,
                destination: _,
                unwind: _,
            } => {
                for op in operands {
                    match *op {
                        InlineAsmOperand::In { reg: _, ref value }
                         => {
                            self.gather_operand(value);
                        }
                        InlineAsmOperand::Out { reg: _, late: _, place, .. } => {
                            if let Some(place) = place {
                                self.create_move_path(place);
                                self.gather_init(place.as_ref(), InitKind::Deep);
                            }
                        }
                        InlineAsmOperand::InOut { reg: _, late: _, ref in_value, out_place } => {
                            self.gather_operand(in_value);
                            if let Some(out_place) = out_place {
                                self.create_move_path(out_place);
                                self.gather_init(out_place.as_ref(), InitKind::Deep);
                            }
                        }
                        InlineAsmOperand::Const { value: _ }
                        | InlineAsmOperand::SymFn { value: _ }
                        | InlineAsmOperand::SymStatic { def_id: _ } => {}
                    }
                }
            }
        }
    }

    fn gather_operand(&mut self, operand: &Operand<'tcx>) {
        match *operand {
            Operand::Constant(..) | Operand::Copy(..) => {} // not-a-move
            Operand::Move(place) => {
                // a move
                self.gather_move(place);
            }
        }
    }

    fn gather_move(&mut self, place: Place<'tcx>) {
        debug!("gather_move({:?}, {:?})", self.loc, place);
        if let [ref base @ .., ProjectionElem::Subslice { from, to, from_end: false }] =
            **place.projection
        {
            // Split `Subslice` patterns into the corresponding list of
            // `ConstIndex` patterns. This is done to ensure that all move paths
            // are disjoint, which is expected by drop elaboration.
            let base_place =
                Place { local: place.local, projection: self.builder.tcx.mk_place_elems(base) };
            let base_path = match self.move_path_for(base_place) {
                Ok(path) => path,
                Err(MoveError::UnionMove { path }) => {
                    self.record_move(place, path);
                    return;
                }
                Err(MoveError::IllegalMove { .. } | MoveError::UntrackedLocal) => return,
            };
            let base_ty = base_place.ty(self.builder.body, self.builder.tcx).ty;
            let len: u64 = match base_ty.kind() {
                ty::Array(_, size) => {
                    size.eval_target_usize(self.builder.tcx, self.builder.param_env)
                }
                _ => bug!("from_end: false slice pattern of non-array type"),
            };
            for offset in from..to {
                let elem =
                    ProjectionElem::ConstantIndex { offset, min_length: len, from_end: false };
                let path =
                    self.add_move_path(base_path, elem, |tcx| tcx.mk_place_elem(base_place, elem));
                self.record_move(place, path);
            }
        } else {
            match self.move_path_for(place) {
                Ok(path) | Err(MoveError::UnionMove { path }) => self.record_move(place, path),
                Err(MoveError::IllegalMove { .. } | MoveError::UntrackedLocal) => {}
            };
        }
    }

    fn record_move(&mut self, place: Place<'tcx>, path: MovePathIndex) {
        let move_out = self.builder.data.moves.push(MoveOut { path, source: self.loc });
        debug!(
            "gather_move({:?}, {:?}): adding move {:?} of {:?}",
            self.loc, place, move_out, path
        );
        self.builder.data.path_map[path].push(move_out);
        self.builder.data.loc_map[self.loc].push(move_out);
    }

    fn gather_init(&mut self, place: PlaceRef<'tcx>, kind: InitKind) {
        debug!("gather_init({:?}, {:?})", self.loc, place);

        let mut place = place;

        // Check if we are assigning into a field of a union, if so, lookup the place
        // of the union so it is marked as initialized again.
        if let Some((place_base, ProjectionElem::Field(_, _))) = place.last_projection() {
            if place_base.ty(self.builder.body, self.builder.tcx).ty.is_union() {
                place = place_base;
            }
        }

        if let LookupResult::Exact(path) = self.builder.data.rev_lookup.find(place) {
            let init = self.builder.data.inits.push(Init {
                location: InitLocation::Statement(self.loc),
                path,
                kind,
            });

            debug!(
                "gather_init({:?}, {:?}): adding init {:?} of {:?}",
                self.loc, place, init, path
            );

            self.builder.data.init_path_map[path].push(init);
            self.builder.data.init_loc_map[self.loc].push(init);
        }
    }
}
