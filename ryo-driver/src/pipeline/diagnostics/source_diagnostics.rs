use ariadne::{Cache, Color, Label, Report, ReportKind, Source as AriadneSource};
use ryo_core::diag::{Diag, DiagCode, Severity};
use ryo_core::errors::CompilerError;
use ryo_core::source::{SourceDb, SourceId};
use ryo_core::span::Span;
use std::collections::HashMap;
use std::ops::Range;

struct RenderSource<'a> {
    source: AriadneSource<&'a str>,
    byte_to_char: Vec<usize>,
}

struct RenderCache<'a> {
    sources: &'a SourceDb,
    entries: HashMap<SourceId, RenderSource<'a>>,
}

impl<'a> RenderCache<'a> {
    fn new(sources: &'a SourceDb) -> Self {
        Self {
            sources,
            entries: HashMap::new(),
        }
    }

    fn char_span(&mut self, span: Span) -> Result<Range<usize>, CompilerError> {
        self.sources.validate_span(span)?;
        let id = span.source;
        if !self.entries.contains_key(&id) {
            let source = self.sources.get(id)?;
            let text = source.text();
            let render_input = if text.len() > u32::MAX as usize {
                &text[..0]
            } else {
                text
            };
            self.entries.insert(
                id,
                RenderSource {
                    source: AriadneSource::from(render_input),
                    byte_to_char: byte_to_char_offsets(render_input),
                },
            );
        }
        let entry = &self.entries[&id];
        Ok(to_char_span(&entry.byte_to_char, span.range()))
    }
}

impl<'a> Cache<SourceId> for RenderCache<'a> {
    type Storage = &'a str;

    fn fetch(
        &mut self,
        id: &SourceId,
    ) -> Result<&AriadneSource<Self::Storage>, impl std::fmt::Debug> {
        self.entries
            .get(id)
            .map(|entry| &entry.source)
            .ok_or_else(|| "render source was not prepared".to_string())
    }

    fn display<'b>(&self, id: &'b SourceId) -> Option<impl std::fmt::Display + 'b> {
        self.sources
            .get(*id)
            .ok()
            .map(|source| source.name().to_string_lossy().into_owned())
    }
}

fn byte_to_char_offsets(input: &str) -> Vec<usize> {
    let mut offsets = vec![0; input.len() + 1];
    let mut chars = 0;
    for (byte, _) in input.char_indices() {
        offsets[byte] = chars;
        chars += 1;
    }
    offsets[input.len()] = chars;
    offsets
}

fn to_char_span(offsets: &[usize], span: Range<usize>) -> Range<usize> {
    let fallback = *offsets
        .last()
        .expect("the table always has the len+1 entry");
    offsets.get(span.start).copied().unwrap_or(fallback)
        ..offsets.get(span.end).copied().unwrap_or(fallback)
}

fn color_for_severity(severity: Severity) -> Color {
    match severity {
        Severity::Error => Color::Red,
        Severity::Warning => Color::Yellow,
        Severity::Note => Color::Blue,
    }
}

fn diag_code(code: DiagCode) -> &'static str {
    super::diag_code_str(code)
}

fn emit_source_less<W: std::io::Write>(diag: &Diag, out: &mut W) -> Result<(), CompilerError> {
    if diag
        .notes
        .iter()
        .any(|note| note.span.is_some_and(|span| span != Span::EMPTY))
    {
        return Err(CompilerError::InternalError(
            "source-less diagnostic has a source-located note".into(),
        ));
    }
    let code = diag_code(diag.code);
    crate::output::write_and_flush(&mut *out, format_args!("{}: {}\n", code, diag.message))?;
    for note in &diag.notes {
        crate::output::write_and_flush(&mut *out, format_args!("{}\n", note.message))?;
    }
    Ok(())
}

fn emit_one<W: std::io::Write>(
    diag: &Diag,
    cache: &mut RenderCache,
    out: &mut W,
) -> Result<(), CompilerError> {
    if diag.span == Span::EMPTY {
        return emit_source_less(diag, out);
    }
    let kind = match diag.severity {
        Severity::Error => ReportKind::Error,
        Severity::Warning => ReportKind::Warning,
        Severity::Note => ReportKind::Advice,
    };
    let label_color = color_for_severity(diag.severity);
    let code = diag_code(diag.code);
    let span = cache.char_span(diag.span)?;
    let source = diag.span.source;
    let mut report = Report::build(kind, (source, span.clone()))
        .with_code(code)
        .with_message(&diag.message)
        .with_label(Label::new((source, span)).with_color(label_color));
    for note in &diag.notes {
        match note.span {
            Some(span) if span != Span::EMPTY => {
                let source = span.source;
                let char_span = cache.char_span(span)?;
                report = report.with_label(
                    Label::new((source, char_span))
                        .with_message(&note.message)
                        .with_color(Color::Cyan),
                );
            }
            _ => report = report.with_note(&note.message),
        }
    }
    match report.finish().write(cache, &mut *out) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(_) => crate::output::write_and_flush(out, format_args!("{}: {}\n", code, diag.message)),
    }
}

pub(crate) fn render_diags_with_writer<W: std::io::Write>(
    diags: &[Diag],
    sources: &SourceDb,
    out: &mut W,
) -> Result<(), CompilerError> {
    let mut cache = RenderCache::new(sources);
    let (truncation, regular): (Vec<&Diag>, Vec<&Diag>) = diags
        .iter()
        .partition(|diag| diag.code == DiagCode::TooManyDiagnostics);
    let mut sorted = regular;
    sorted.sort_by_key(|diag| (diag.span.start, diag.span.end));
    for diag in sorted {
        emit_one(diag, &mut cache, out)?;
    }
    for diag in truncation {
        emit_one(diag, &mut cache, out)?;
    }
    Ok(())
}

pub(crate) fn render_diags(diags: &[Diag], sources: &SourceDb) -> Result<(), CompilerError> {
    render_diags_with_writer(diags, sources, &mut std::io::stderr().lock())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_to_char_offsets_map_boundaries() {
        let ascii = byte_to_char_offsets("ab\ncd");
        assert_eq!(ascii, vec![0, 1, 2, 3, 4, 5]);

        let emoji = "\u{1F600}x";
        let table = byte_to_char_offsets(emoji);
        assert_eq!(table.len(), emoji.len() + 1);
        assert_eq!(to_char_span(&table, 0..4), 0..1);
        assert_eq!(to_char_span(&table, 4..5), 1..2);
        assert_eq!(to_char_span(&table, 0..emoji.len()), 0..2);
        assert_eq!(to_char_span(&table, 5..9), 2..2);
    }
}
