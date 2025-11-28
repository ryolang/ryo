/**
 * Ryo Programming Language Syntax Highlighter for highlight.js
 *
 * @param {import('highlight.js/lib/core')} hljs
 *   The highlight.js library instance.
 * @returns {import('highlight.js').Language}
 *   The Ryo language definition.
 */
const ryo = function (hljs) {
  const KEYWORDS = {
    $pattern: /[a-zA-Z_][a-zA-Z0-9_]*/,
    keyword: [
      "and",
      "as",
      "break",
      "case",
      "catch",
      "continue",
      "elif",
      "else",
      "enum",
      "error",
      "fn",
      "for",
      "if",
      "impl",
      "import",
      "in",
      "match",
      "move",
      "mut",
      "not",
      "or",
      "orelse",
      "package",
      "pub",
      "return",
      "select",
      "struct",
      "trait",
      "try",
    ],
    literal: ["true", "false", "none"],
    type: [
      "list",
      "map",
      "shared",
      "weak",
      "bool",
      "char",
      "f32",
      "f64",
      "float",
      "i16",
      "i32",
      "i64",
      "i8",
      "int",
      "isize",
      "str",
      "u16",
      "u32",
      "u64",
      "u8",
      "uint",
      "usize",
      "void",
    ],
    built_in: ["self", "panic", "print", "println"],
  };

  const DOC_COMMENT = {
    scope: "doctag",
    begin: /#:/,
    end: /$/,
    relevance: 10,
  };
  const COMMENT = hljs.COMMENT("#", "$");

  // IMPROVED F-STRING HANDLING
  const F_STRING_INTERPOLATION = {
    scope: "subst",
    begin: /\{/,
    end: /\}/,
    keywords: KEYWORDS,
    contains: [], // Will be populated later to allow nesting
  };

  const F_STRING = {
    scope: "string",
    begin: /f"/,
    end: /"/,
    contains: [hljs.BACKSLASH_ESCAPE, F_STRING_INTERPOLATION],
  };

  const NUMBER = {
    scope: "number",
    variants: [
      { begin: "\\b(0b[01_]+)" },
      { begin: "\\b(0o[0-7_]+)" },
      { begin: "\\b(0x[a-fA-F0-9_]+)" },
      { begin: "\\b(\\d[\\d_]*(\\.[0-9_]+)?([eE][+-]?[0-9_]+)?)" },
    ],
    relevance: 0,
  };

  // Allow f-strings and numbers inside f-string interpolations
  F_STRING_INTERPOLATION.contains.push(
    F_STRING,
    hljs.QUOTE_STRING_MODE,
    NUMBER,
  );

  const ATTRIBUTE = {
    scope: "meta",
    begin: /#\[/,
    end: /\]/,
  };

  const PASCAL_CASE_TYPE = {
    scope: "type",
    begin: "\\b[A-Z][a-zA-Z0-9_]*",
    relevance: 0,
  };

  const TYPE_DEF = {
    begin: [/(struct|enum|trait)/, /\s+/, /[A-Z_][a-zA-Z0-9_]*/],
    beginScope: {
      1: "keyword",
      3: "title.class",
    },
  };

  const FUNCTION_DEF = {
    begin: [/fn/, /\s+/, /[a-zA-Z_][a-zA-Z0-9_]*/],
    beginScope: {
      1: "keyword",
      3: "title.function",
    },
    relevance: 0,
    contains: [
      {
        begin: /\(/,
        end: /\)/,
        keywords: KEYWORDS,
        contains: [COMMENT, PASCAL_CASE_TYPE],
      },
      {
        begin: /->/,
        endsParent: false,
        contains: [PASCAL_CASE_TYPE],
      },
    ],
  };

  const IMPL_DEF = {
    begin: /impl\b/,
    end: /:/,
    keywords: KEYWORDS,
    contains: [COMMENT, PASCAL_CASE_TYPE],
    relevance: 10,
  };

  const PREFIXED_TYPE = {
    begin: /(&mut\s*|&\s*|!|\?)\s*/,
    keywords: KEYWORDS,
    contains: [PASCAL_CASE_TYPE],
    relevance: 1.5,
  };

  return {
    name: "Ryo",
    aliases: ["ryo"],
    keywords: KEYWORDS,
    illegal: /<\//,
    contains: [
      DOC_COMMENT,
      COMMENT,
      ATTRIBUTE,
      F_STRING,
      hljs.QUOTE_STRING_MODE,
      hljs.APOS_STRING_MODE,
      NUMBER,
      IMPL_DEF,
      FUNCTION_DEF,
      TYPE_DEF,
      PREFIXED_TYPE,
      PASCAL_CASE_TYPE,
    ],
  };
};
