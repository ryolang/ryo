//! TypeMismatch enrichment for struct-kinded types (M10 §9 message
//! bar): a field-level diff note when both sides are named or
//! anonymous structs, plus the graduation fix-it when the expected
//! side is a named struct and the found side is an anonymous shape.

use ryo_core::diag::{Diag, DiagNote};
use ryo_core::types::{InternPool, TypeId, TypeKind};

/// Attach the struct-shape diff notes (if any) to an already-built
/// TypeMismatch diagnostic. Every mismatch site can call this
/// unconditionally — non-struct types yield no notes, so existing
/// messages for primitives etc. are untouched.
pub(crate) fn with_struct_shape_notes(
    mut diag: Diag,
    pool: &InternPool,
    expected: TypeId,
    found: TypeId,
) -> Diag {
    diag.notes.extend(struct_shape_notes(pool, expected, found));
    diag
}

/// Field-level diff between two struct-kinded types:
///
/// - Same field count: one note per position whose (name, type) pair
///   differs. A name diff reads `fields differ at position N:
///   expected 'rr', found 'r'; did you mean 'r'?` — the found name is
///   the one the value actually carries, so the suggestion aligns the
///   annotation to it. A type diff names the field: `field 'q':
///   expected 'int', found 'str'`.
/// - Different field count: the found shape's missing/extra fields
///   are named (the full shapes already render in the main message).
/// - Graduation: expected named struct + found anonymous shape gets
///   `help: construct explicitly: \`Name{f=…, …}\`` — the real struct
///   name and its real field names, in declaration order.
fn struct_shape_notes(pool: &InternPool, expected: TypeId, found: TypeId) -> Vec<DiagNote> {
    let struct_kinded =
        |ty: TypeId| matches!(pool.kind(ty), TypeKind::Struct | TypeKind::AnonStruct);
    if !struct_kinded(expected) || !struct_kinded(found) {
        return Vec::new();
    }
    // A declared-but-undefined struct (failed definition) has no
    // readable layout; the original diagnostic already explains the
    // failure, so don't pile shape notes on top.
    if (matches!(pool.kind(expected), TypeKind::Struct) && !pool.is_defined_struct(expected))
        || (matches!(pool.kind(found), TypeKind::Struct) && !pool.is_defined_struct(found))
    {
        return Vec::new();
    }
    let ev = pool.struct_view(expected);
    let fv = pool.struct_view(found);
    let mut notes = Vec::new();
    if ev.fields.len() == fv.fields.len() {
        for (i, (ef, ff)) in ev.fields.iter().zip(&fv.fields).enumerate() {
            if ef.name == ff.name && ef.ty == ff.ty {
                continue;
            }
            let message = if ef.name != ff.name {
                format!(
                    "fields differ at position {}: expected '{}', found '{}'; \
                     did you mean '{}'?",
                    i + 1,
                    pool.str(ef.name),
                    pool.str(ff.name),
                    pool.str(ff.name),
                )
            } else {
                format!(
                    "field '{}': expected '{}', found '{}'",
                    pool.str(ef.name),
                    pool.display(ef.ty),
                    pool.display(ff.ty),
                )
            };
            notes.push(DiagNote {
                span: None,
                message,
            });
        }
    } else {
        let found_names: std::collections::HashSet<_> = fv.fields.iter().map(|f| f.name).collect();
        let expected_names: std::collections::HashSet<_> =
            ev.fields.iter().map(|f| f.name).collect();
        let missing: Vec<String> = ev
            .fields
            .iter()
            .filter(|f| !found_names.contains(&f.name))
            .map(|f| format!("'{}'", pool.str(f.name)))
            .collect();
        let extra: Vec<String> = fv
            .fields
            .iter()
            .filter(|f| !expected_names.contains(&f.name))
            .map(|f| format!("'{}'", pool.str(f.name)))
            .collect();
        if !missing.is_empty() {
            notes.push(DiagNote {
                span: None,
                message: format!("the found shape is missing field(s) {}", missing.join(", ")),
            });
        }
        if !extra.is_empty() {
            notes.push(DiagNote {
                span: None,
                message: format!(
                    "the found shape has field(s) {} that the expected shape does not",
                    extra.join(", ")
                ),
            });
        }
    }
    // Graduation: an anonymous shape used where a named struct is
    // expected has no implicit coercion — construct the named struct
    // explicitly with its real field names.
    if matches!(pool.kind(expected), TypeKind::Struct)
        && matches!(pool.kind(found), TypeKind::AnonStruct)
    {
        let name = pool.str(ev.name);
        let fields = ev
            .fields
            .iter()
            .map(|f| format!("{}=…", pool.str(f.name)))
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(DiagNote {
            span: None,
            message: format!("help: construct explicitly: `{name}{{{fields}}}`"),
        });
    }
    notes
}
