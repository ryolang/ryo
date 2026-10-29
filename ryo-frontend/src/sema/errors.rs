use super::Sema;
use ryo_core::diag::{Diag, DiagCode};

pub(super) fn register_errors(sema: &mut Sema<'_>) {
    for decl in &sema.uir.error_decls {
        for field in &decl.payload.fields {
            if sema.pool.is_view(field.ty) {
                sema.sink.emit(Diag::error(
                    field.span,
                    DiagCode::ViewFieldType,
                    format!(
                        "error payload fields must be owned values; '{}' is a projection (Rule 6)",
                        sema.pool.display(field.ty),
                    ),
                ));
            }
        }
        sema.error_types.entry(decl.name).or_insert(decl.ty);
    }
}
