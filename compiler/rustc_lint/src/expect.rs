use crate::context::{CheckLintNameResult, LintStore};
use crate::levels::try_parse_reason_metadata;
use rustc_ast as ast;
use rustc_ast::unwrap_or;
use rustc_ast_pretty::pprust;
use rustc_errors::LintEmission;
use rustc_hir as hir;
use rustc_hir::intravisit;
use rustc_middle::hir::map::Map;
use rustc_middle::ty::query::Providers;
use rustc_middle::ty::TyCtxt;
use rustc_session::lint::LintId;
use rustc_session::Session;
use rustc_span::symbol::{sym, Symbol};
use rustc_span::Span;

fn check_expect_lint(_tcx: TyCtxt<'_>, (): ()) -> () {
    //    let store = unerased_lint_store(tcx);
    //    let crate_attrs = tcx.hir().attrs(CRATE_HIR_ID);
    //    let levels = LintLevelsBuilder::new(tcx.sess, false, &store, crate_attrs);
    //    let mut builder = LintLevelMapBuilder { levels, tcx, store };
    //    let krate = tcx.hir().krate();
    //
    //    builder.levels.id_to_set.reserve(krate.exported_macros.len() + 1);
    //
    //    let push = builder.levels.push(tcx.hir().attrs(hir::CRATE_HIR_ID), &store, true);
    //    builder.levels.register_id(hir::CRATE_HIR_ID);
    //    for macro_def in krate.exported_macros {
    //        builder.levels.register_id(macro_def.hir_id());
    //    }
    //    intravisit::walk_crate(&mut builder, krate);
    //    builder.levels.pop(push);
}

#[derive(Debug, Clone)]
struct LintExpectation {
    lints: Vec<LintId>,
    reason: Option<Symbol>,
    attr_span: Span,
}

impl LintExpectation {
    fn new(lints: Vec<LintId>, reason: Option<Symbol>, attr_span: Span) -> Self {
        Self { lints, reason, attr_span }
    }
}

struct ExpectLintChecker<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    sess: &'a Session,
    store: &'a LintStore,
    #[allow(unused)]
    emitted_lints: Vec<LintEmission>,
}

impl<'a, 'tcx> ExpectLintChecker<'a, 'tcx> {
    #[allow(unused)]
    fn new(tcx: TyCtxt<'tcx>, sess: &'a Session, store: &'a LintStore) -> Self {
        let emitted_lints = tcx.sess.diagnostic().steal_expect_lint_emissions();

        Self { tcx, sess, store, emitted_lints }
    }

    fn check_item_with_attrs<F>(&mut self, id: hir::HirId, scope: Span, f: F)
    where
        F: FnOnce(&mut Self),
    {
        let mut expectations = self.collect_expectations(id);

        f(self);

        for expect in expectations.drain(..) {
            self.check_expectation(expect, scope);
        }
    }

    fn collect_expectations(&self, id: hir::HirId) -> Vec<LintExpectation> {
        let mut result = Vec::new();

        for attr in self.tcx.hir().attrs(id) {
            // We only care about expectations
            if attr.name_or_empty() != sym::expect {
                continue;
            }

            self.sess.mark_attr_used(attr);

            let mut metas = unwrap_or!(attr.meta_item_list(), continue);
            if metas.is_empty() {
                // FIXME (#55112): issue unused-attributes lint for `#[level()]`
                continue;
            }

            // Before processing the lint names, look for a reason (RFC 2383)
            // at the end.
            let tail_li = &metas[metas.len() - 1];
            let reason = try_parse_reason_metadata(tail_li, self.sess);
            if reason.is_some() {
                // found reason, reslice meta list to exclude it
                metas.pop().unwrap();
            }

            // This will simply collect the lints specified in the expect attribute.
            // Error handling about unknown renamed and weird lints is done by the
            // `LintLevelMapBuilder`
            let mut lints: Vec<LintId> = Default::default();
            for li in metas {
                let mut meta_item = match li {
                    ast::NestedMetaItem::MetaItem(meta_item) if meta_item.is_word() => meta_item,
                    _ => continue,
                };

                // Extracting the tool
                let tool_name = if meta_item.path.segments.len() > 1 {
                    Some(meta_item.path.segments.remove(0).ident.name)
                } else {
                    None
                };

                // Checking the lint name
                let name = pprust::path_to_string(&meta_item.path);
                match &self.store.check_lint_name(&name, tool_name) {
                    CheckLintNameResult::Ok(ids) => {
                        lints.extend_from_slice(ids);
                    }
                    CheckLintNameResult::Tool(result) => {
                        match *result {
                            Ok(ids) => {
                                lints.extend_from_slice(ids);
                            }
                            Err((_, _)) => {
                                // The lint could not be found, this can happen if the
                                // lint doesn't exist in the tool or if the Tool is not
                                // enabled. In either case we don't want to add it to the
                                // lints as it can not be emitted during this compiler run
                                // and the expectation could therefor also not be fulfilled.
                                continue;
                            }
                        }
                    }
                    CheckLintNameResult::Warning(_, Some(new_name)) => {
                        // The lint has been renamed. The `LintLevelMapBuilder` then
                        // registers the level for the new name. This means that the
                        // expectation of a renamed lint should also be fulfilled by
                        // the new name of the lint.

                        // NOTE: `new_name` already includes the tool name, so we don't have to add it again.
                        if let CheckLintNameResult::Ok(ids) =
                            self.store.check_lint_name(&new_name, None)
                        {
                            lints.extend_from_slice(ids);
                        }
                    }
                    CheckLintNameResult::Warning(_, _) | CheckLintNameResult::NoLint(_) => {
                        // The `LintLevelMapBuilder` will issue a message about this.
                        continue;
                    }
                }
            }

            if !lints.is_empty() {
                result.push(LintExpectation::new(lints, reason, attr.span));
            }
        }

        result
    }

    #[allow(unused)]
    fn check_expectation(&self, expectation: LintExpectation, scope: Span) {
        todo!()
    }
}

impl<'tcx> intravisit::Visitor<'tcx> for ExpectLintChecker<'_, 'tcx> {
    type Map = Map<'tcx>;

    fn nested_visit_map(&mut self) -> intravisit::NestedVisitorMap<Self::Map> {
        intravisit::NestedVisitorMap::All(self.tcx.hir())
    }

    fn visit_param(&mut self, param: &'tcx hir::Param<'tcx>) {
        self.check_item_with_attrs(param.hir_id, param.span, |builder| {
            intravisit::walk_param(builder, param);
        });
    }

    fn visit_item(&mut self, it: &'tcx hir::Item<'tcx>) {
        self.check_item_with_attrs(it.hir_id(), it.span, |builder| {
            intravisit::walk_item(builder, it);
        });
    }

    fn visit_foreign_item(&mut self, it: &'tcx hir::ForeignItem<'tcx>) {
        self.check_item_with_attrs(it.hir_id(), it.span, |builder| {
            intravisit::walk_foreign_item(builder, it);
        })
    }

    fn visit_stmt(&mut self, e: &'tcx hir::Stmt<'tcx>) {
        // We will call `with_lint_attrs` when we walk
        // the `StmtKind`. The outer statement itself doesn't
        // define the lint levels.
        intravisit::walk_stmt(self, e);
    }

    fn visit_expr(&mut self, e: &'tcx hir::Expr<'tcx>) {
        self.check_item_with_attrs(e.hir_id, e.span, |builder| {
            intravisit::walk_expr(builder, e);
        })
    }

    fn visit_field_def(&mut self, s: &'tcx hir::FieldDef<'tcx>) {
        self.check_item_with_attrs(s.hir_id, s.span, |builder| {
            intravisit::walk_field_def(builder, s);
        })
    }

    fn visit_variant(
        &mut self,
        v: &'tcx hir::Variant<'tcx>,
        g: &'tcx hir::Generics<'tcx>,
        item_id: hir::HirId,
    ) {
        self.check_item_with_attrs(v.id, v.span, |builder| {
            intravisit::walk_variant(builder, v, g, item_id);
        })
    }

    fn visit_local(&mut self, l: &'tcx hir::Local<'tcx>) {
        self.check_item_with_attrs(l.hir_id, l.span, |builder| {
            intravisit::walk_local(builder, l);
        })
    }

    fn visit_arm(&mut self, a: &'tcx hir::Arm<'tcx>) {
        self.check_item_with_attrs(a.hir_id, a.span, |builder| {
            intravisit::walk_arm(builder, a);
        })
    }

    fn visit_trait_item(&mut self, trait_item: &'tcx hir::TraitItem<'tcx>) {
        self.check_item_with_attrs(trait_item.hir_id(), trait_item.span, |builder| {
            intravisit::walk_trait_item(builder, trait_item);
        });
    }

    fn visit_impl_item(&mut self, impl_item: &'tcx hir::ImplItem<'tcx>) {
        self.check_item_with_attrs(impl_item.hir_id(), impl_item.span, |builder| {
            intravisit::walk_impl_item(builder, impl_item);
        });
    }
}

pub fn provide(providers: &mut Providers) {
    providers.check_expect_lint = check_expect_lint;
}
