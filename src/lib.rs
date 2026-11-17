#![feature(box_patterns)]

#[macro_use]
extern crate lazy_static;

use base64::{engine::general_purpose, Engine as _};
use serde::Serialize;
use std::fmt;
use std::path::PathBuf;
use swc_common::comments::SingleThreadedComments;
use swc_common::source_map::SourceMapGenConfig;
use swc_common::{self, sync::Lrc, FileName, Mark, SourceFile, SourceMap, Span, Spanned};
use swc_core::common::GLOBALS;
use swc_ecma_ast::{
    Ident, ImportDecl, ImportNamedSpecifier, ImportSpecifier, Module, ModuleDecl, ModuleExportName,
    ModuleItem,
};
use swc_ecma_codegen::Emitter;
use swc_ecma_parser::lexer::scan_content_tags;
pub use swc_ecma_parser::lexer::{
    find_first_closing_tag, ContentTagError, ContentTagParser, OpenTag,
};
use swc_ecma_parser::TsSyntax;
use swc_ecma_parser::{lexer::Lexer, Parser, StringInput, Syntax};
use swc_ecma_transforms::resolver;
use swc_ecma_utils::private_ident;
use swc_ecma_visit::{VisitMutWith, VisitWith};

mod bindings;
mod locate;
mod snippets;
mod splice;
mod transform;

pub use locate::Occurrence;
pub use splice::{Imports, SpliceError, Spliced, SplicedTag};

#[derive(Default)]
pub struct Options {
    pub filename: Option<PathBuf>,
    pub inline_source_map: bool,
}

/// Reads content tags. It holds only its settings, so one preprocessor can be
/// reused for any number of files. Each call to `parse` or `process` keeps its
/// source map and comments to itself, and drops them when it returns.
pub struct Preprocessor {
    parser: Box<dyn ContentTagParser>,
}

/// A parse error, with the source map needed to show where it is.
pub struct ParseError {
    pub error: swc_ecma_parser::error::Error,
    pub source_map: Lrc<SourceMap>,
}

// `SourceMap` does not implement `Debug`, so only the error is shown.
impl fmt::Debug for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl From<ParseError> for swc_ecma_parser::error::Error {
    fn from(err: ParseError) -> Self {
        err.error
    }
}

/// What [Preprocessor::parse_with_recovery] found.
#[derive(Debug, Serialize)]
pub struct Recovered {
    pub templates: Vec<locate::Occurrence>,
    /// Empty when the file parsed. Otherwise the parse error comes first,
    /// followed by any errors from finding the content tags without parsing.
    pub errors: Vec<RecoveryError>,
}

/// An error found by [Preprocessor::parse_with_recovery].
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct RecoveryError {
    pub kind: RecoveryErrorKind,
    pub message: String,
    /// For [RecoveryErrorKind::MissingClosingTag], the opening tag.
    pub range: locate::Range,
}

/// What a [RecoveryError] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryErrorKind {
    /// The JavaScript or TypeScript outside the content tags does not parse.
    Syntax,
    /// A content tag has no closing tag. Its end is then a guess, or the tag
    /// is not in `templates` at all.
    MissingClosingTag,
    /// The parser could not read the content of a content tag, such as a
    /// comment in it that is not closed. The tag still ends at a closing tag.
    TagContent,
}

impl RecoveryError {
    fn new(error: &swc_ecma_parser::error::Error, range: locate::Range) -> Self {
        use swc_ecma_parser::error::SyntaxError;
        let kind = match error.kind() {
            SyntaxError::ContentTagNotClosed(_) => RecoveryErrorKind::MissingClosingTag,
            SyntaxError::ContentTag(_) | SyntaxError::ContentEndIsNotClosingTag { .. } => {
                RecoveryErrorKind::TagContent
            }
            _ => RecoveryErrorKind::Syntax,
        };
        Self {
            kind,
            message: error.kind().msg().into_owned(),
            range,
        }
    }
}

/// The settings for [Preprocessor::with_config].
#[derive(Default)]
pub struct Config {
    /// Which content tags to read, and where each one ends. The default is
    /// [TagNames] with `template`.
    pub parser: Option<Box<dyn ContentTagParser>>,
}

/// A [ContentTagParser] that only lists tag names, so each content tag ends at
/// its first closing tag.
pub struct TagNames(Vec<String>);

impl TagNames {
    pub fn new<S: Into<String>>(tags: impl IntoIterator<Item = S>) -> Self {
        Self(tags.into_iter().map(Into::into).collect())
    }
}

impl ContentTagParser for TagNames {
    fn tags(&self) -> &[String] {
        &self.0
    }
}

/// The settings given to [Preprocessor::with_config] are not allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidConfig {
    /// The parser lists no tags.
    NoTags,
    /// The parser lists a tag name that is not allowed.
    InvalidTagName(String),
}

impl fmt::Display for InvalidConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InvalidConfig::NoTags => write!(f, "The parser must list at least one tag."),
            InvalidConfig::InvalidTagName(tag) => write!(
                f,
                "Invalid content tag name {tag:?}. A tag name must start with an ASCII letter and contain only ASCII letters, digits and hyphens."
            ),
        }
    }
}

impl std::error::Error for InvalidConfig {}

fn is_valid_tag_name(tag: &str) -> bool {
    let mut chars = tag.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '-')
        }
        _ => false,
    }
}

pub struct CodeMapPair {
    pub code: String,
    pub map: String,
}

/// The end of each local name that is made for an import, so the name cannot
/// clash with a name in the file.
pub const RENAME_SUFFIX: &str = "fd9b2463e5f141cfb5666b64daa1f11a";

/// The local name of `template` from `@ember/template-compiler` in the
/// built-in output.
pub const IMPORT_ALIAS: &str = "template_fd9b2463e5f141cfb5666b64daa1f11a";

struct SourceMapConfig;
impl SourceMapGenConfig for SourceMapConfig {
    fn file_name_to_source(&self, f: &swc_common::FileName) -> String {
        f.to_string()
    }

    fn inline_sources_content(&self, _: &swc_common::FileName) -> bool {
        true
    }
}

impl Preprocessor {
    /// Creates a preprocessor that reads `<template>` content tags, ends each
    /// one at its first closing tag, and writes the Ember output for them.
    pub fn new() -> Self {
        Self::with_config(Config::default())
            .expect("the default config is valid, which the default_config_is_valid test checks")
    }

    pub fn with_config(config: Config) -> Result<Self, InvalidConfig> {
        let parser = config
            .parser
            .unwrap_or_else(|| Box::new(TagNames::new(["template"])));
        let tags = parser.tags();
        if tags.is_empty() {
            return Err(InvalidConfig::NoTags);
        }
        if let Some(tag) = tags.iter().find(|tag| !is_valid_tag_name(tag)) {
            return Err(InvalidConfig::InvalidTagName(tag.clone()));
        }

        Ok(Self { parser })
    }

    /// The tag names this preprocessor reads.
    pub fn tags(&self) -> &[String] {
        self.parser.tags()
    }

    fn lexer<'a>(
        &'a self,
        source_file: &'a SourceFile,
        comments: &'a SingleThreadedComments,
    ) -> Lexer<'a> {
        Lexer::new(
            Syntax::Typescript(TsSyntax {
                decorators: true,
                ..Default::default()
            }),
            Default::default(),
            StringInput::from(source_file),
            Some(comments),
        )
        .with_content_tag_parser(&*self.parser)
    }

    pub fn parse(
        &self,
        src: &str,
        options: Options,
    ) -> Result<Vec<locate::Occurrence>, ParseError> {
        let source_map: Lrc<SourceMap> = Default::default();
        let comments = SingleThreadedComments::default();
        let source_file = new_source_file(&source_map, src, options.filename);

        let mut parser = Parser::new_from(self.lexer(&source_file, &comments));
        GLOBALS.set(&Default::default(), || {
            let parsed_module = parser.parse_module().map_err(|error| ParseError {
                error,
                source_map: source_map.clone(),
            })?;

            let mut visitor =
                locate::LocateContentTagVisitor::new(src.to_string(), source_file.start_pos);

            parsed_module.visit_with(&mut visitor);

            Ok(visitor.occurrences)
        })
    }

    /// Finds the content tags in `src` even when it has a syntax error, and
    /// never fails.
    ///
    /// When `src` parses, this gives the same templates as [Self::parse] and
    /// no errors. When it does not, the parser has no syntax tree to give, so
    /// the content tags are found with swc's lexer alone. Those templates
    /// have the same ranges and contents, but whether each one is a class
    /// member is a guess, and a regex in an unusual place can be misread.
    pub fn parse_with_recovery(&self, src: &str, options: Options) -> Recovered {
        let parse_error = match self.parse(src, options) {
            Ok(templates) => {
                return Recovered {
                    templates,
                    errors: vec![],
                }
            }
            Err(err) => err,
        };
        let is_ascii = src.is_ascii();
        let error_range = |source_map: &SourceMap, span: Span| {
            let start = source_map.lookup_byte_offset(span.lo).pos.0 as usize;
            let end = source_map.lookup_byte_offset(span.hi).pos.0 as usize;
            locate::Range::from_offsets(src, start.min(src.len()), end.min(src.len()), is_ascii)
        };
        let mut errors = vec![RecoveryError::new(
            &parse_error.error,
            error_range(&parse_error.source_map, parse_error.error.span()),
        )];

        let source_map: Lrc<SourceMap> = Default::default();
        let source_file = new_source_file(&source_map, src, None);
        let scan = GLOBALS.set(&Default::default(), || {
            scan_content_tags(
                Syntax::Typescript(TsSyntax {
                    decorators: true,
                    ..Default::default()
                }),
                StringInput::from(&*source_file),
                &*self.parser,
            )
        });
        for error in scan.errors {
            let error = RecoveryError::new(&error, error_range(&source_map, error.span()));
            if !errors.contains(&error) {
                errors.push(error);
            }
        }

        Recovered {
            templates: scan
                .tags
                .iter()
                .map(|tag| locate::scanned_occurrence(src, tag, is_ascii))
                .collect(),
            errors,
        }
    }

    pub fn process(&self, src: &str, options: Options) -> Result<CodeMapPair, ParseError> {
        let target_specifier = "template";
        let target_module = "@ember/template-compiler";
        let source_map: Lrc<SourceMap> = Default::default();
        let comments = SingleThreadedComments::default();
        let source_file = new_source_file(&source_map, src, options.filename);
        let parse_error = |error| ParseError {
            error,
            source_map: source_map.clone(),
        };

        let mut parser = Parser::new_from(self.lexer(&source_file, &comments));
        GLOBALS.set(&Default::default(), || {
            let mut parsed_module = parser.parse_module().map_err(parse_error)?;

            let id = private_ident!(IMPORT_ALIAS);
            let mut needs_import = false;
            let mut transform = transform::TransformVisitor::new(
                &id,
                Some(&mut needs_import),
                src,
                source_file.start_pos,
            );
            parsed_module.visit_mut_with(&mut transform);
            if let Some(err) = transform.error {
                return Err(parse_error(err));
            }

            if needs_import {
                insert_import(&mut parsed_module, target_module, target_specifier, &id)
            }

            let unresolved_mark = Mark::new();
            let top_level_mark = Mark::new();

            parsed_module.visit_mut_with(&mut resolver(unresolved_mark, top_level_mark, false));

            Ok(print(
                &parsed_module,
                &source_map,
                &comments,
                options.inline_source_map,
            ))
        })
    }
}

fn new_source_file(
    source_map: &Lrc<SourceMap>,
    src: &str,
    filename: Option<PathBuf>,
) -> Lrc<SourceFile> {
    let filename = match filename {
        Some(name) => FileName::Real(name),
        None => FileName::Anon,
    };
    source_map.new_source_file(filename.into(), src.to_string())
}

fn print(
    module: &Module,
    source_map: &Lrc<SourceMap>,
    comments: &SingleThreadedComments,
    inline_source_map: bool,
) -> CodeMapPair {
    let mut buf = vec![];
    let mut srcmap = vec![];
    let mut source_map_buffer = vec![];
    let mut emitter = Emitter {
        cfg: Default::default(),
        cm: source_map.clone(),
        wr: swc_ecma_codegen::text_writer::JsWriter::new(
            source_map.clone(),
            "\n",
            &mut buf,
            Some(&mut srcmap),
        ),
        comments: Some(comments),
    };
    emitter.emit_module(module).unwrap();

    source_map
        .build_source_map(&srcmap, None, SourceMapConfig {})
        .to_writer(&mut source_map_buffer)
        .unwrap();

    if inline_source_map {
        buf.append(&mut encode_inline_source_map(&source_map_buffer).into_bytes());
    }

    let s = String::from_utf8_lossy(&buf);

    CodeMapPair {
        code: s.to_string(),
        map: String::from_utf8(source_map_buffer.clone()).unwrap(),
    }
}

// The comment that holds a source map inside the code it is for.
fn encode_inline_source_map(map: &[u8]) -> String {
    format!(
        "//# sourceMappingURL=data:application/json;base64,{}",
        general_purpose::URL_SAFE_NO_PAD.encode(map)
    )
}

fn insert_import(
    parsed_module: &mut Module,
    target_module: &str,
    target_specifier: &str,
    local: &Ident,
) {
    parsed_module.body.insert(
        0,
        ModuleItem::ModuleDecl(ModuleDecl::Import(ImportDecl {
            span: Default::default(),
            specifiers: vec![ImportSpecifier::Named(ImportNamedSpecifier {
                span: Default::default(),
                local: local.clone(),
                imported: Some(ModuleExportName::Ident(Ident::new_no_ctxt(
                    target_specifier.into(),
                    Default::default(),
                ))),
                is_type_only: false,
            })],
            src: Box::new(target_module.into()),
            type_only: false,
            with: None,
            phase: Default::default(),
        })),
    );
}

#[cfg(test)]
mod test_helpers;

macro_rules! testcase {
    ($test_name:ident, $input:expr, $expected:expr) => {
        #[test]
        fn $test_name() -> Result<(), swc_ecma_parser::error::Error> {
            test_helpers::testcase($input, $expected)
        }
    };
}

testcase! {
  no_preexisting_import,
  r#"let x = <template>hello</template>"#,
  r#"import { template as template_UUID } from "@ember/template-compiler";
     let x = template_UUID(`hello`, { eval() { return eval(arguments[0])} });"#
}

testcase! {
  preexisting_import,
  r#"import { template } from "@ember/template-compiler";
     let x = <template>hello</template>"#,
  r#"import { template as template_UUID } from "@ember/template-compiler";
     import { template } from "@ember/template-compiler";
     let x = template_UUID(`hello`, { eval() { return eval(arguments[0])} });"#
}

testcase! {
  preexisting_renamed_import,
  r#"import { template as t } from "@ember/template-compiler";
     let x = <template>hello</template>"#,
  r#"import { template as template_UUID } from "@ember/template-compiler";
     import { template as t } from "@ember/template-compiler";
     let x = template_UUID(`hello`, { eval() { return eval(arguments[0])} })"#
}

testcase! {
  no_template_tags,
  r#"console.log('hello')"#,
  r#"console.log('hello')"#
}

testcase! {
  avoids_top_level_collision,
  r#"function template() {};
     console.log(template());
     export default <template>Hi</template>"#,
  r#"import { template as template_UUID } from "@ember/template-compiler";
     function template() {};
     console.log(template());
     export default template_UUID(`Hi`, { eval() { return eval(arguments[0])} });"#
}

testcase! {
  avoids_local_collision,
  r#"export default function (template) {
         console.log(template);
         return <template>X</template>;
       };"#,
  r#"import { template as template_UUID } from "@ember/template-compiler";
       export default function(template) {
         console.log(template);
         return template_UUID(`X`, { eval() { return eval(arguments[0])} });
       };"#
}

testcase! {
  handles_typescript,
  r#"function makeComponent(message: string) {
        console.log(message);
        return <template>hello</template>
    }"#,
  r#"import { template as template_UUID } from "@ember/template-compiler";
       function makeComponent(message: string) {
         console.log(message);
         return template_UUID(`hello`, { eval() { return eval(arguments[0]) } });
       }"#
}

testcase! {
  handles_typescript_implied_export_default,
  r#"<template>hello</template> satisfies MyType"#,
  r#"import { template as template_UUID } from "@ember/template-compiler";
export default template_UUID(`hello`, {
    eval () {
        return eval(arguments[0]);
    }
}) satisfies MyType;"#
}

testcase! {
  handles_typescript_this,
  r#"function f(this: Context, ...args: unknown[]) {
        function t(this: Context, ...args: unknown[]) {};
        return <template></template>
    }"#,
  r#"import { template as template_UUID } from "@ember/template-compiler";
       function f(this: Context, ...args: unknown[]) {
         function t(this: Context, ...args: unknown[]) {}
         ;
         return template_UUID(``, { eval() { return eval(arguments[0]) } });
       }"#
}

#[test]
fn preserves_type_reexport() -> Result<(), swc_ecma_parser::error::Error> {
    // we're using a substring test here because the bug I'm testing also effects
    // the normalizer that the other tests are relying on.
    test_helpers::testcase_substr("export { type X } from 'elsewhere';", "type X")
}

testcase! {
  extraneous_indentation_strip,
  r#"let x = <template>
    hello
  </template>"#,
  r#"import { template as template_UUID } from "@ember/template-compiler";
     let x = template_UUID(`hello`, { eval() { return eval(arguments[0])} });"#
}

testcase! {
  multiline_extraneous_indentation_strip,
  r#"let x = <template>
    hello

    extra line break
    <div>
      content
    </div>
  </template>"#,
  r#"import { template as template_UUID } from "@ember/template-compiler";
     let x = template_UUID(`hello

extra line break
<div>
  content
</div>`, { eval() { return eval(arguments[0])} });"#
}

#[cfg(test)]
fn configured(tags: &[&str]) -> Result<Preprocessor, InvalidConfig> {
    Preprocessor::with_config(Config {
        parser: Some(Box::new(TagNames::new(tags.iter().copied()))),
    })
}

// Ends each content tag at its last closing tag in the file.
#[cfg(test)]
struct LastClosingTag(Vec<String>);

#[cfg(test)]
impl ContentTagParser for LastClosingTag {
    fn tags(&self) -> &[String] {
        &self.0
    }

    fn find_end(&self, source: &str, open: &OpenTag) -> Result<usize, ContentTagError> {
        source
            .rfind(&format!("</{}>", open.name))
            .ok_or_else(|| ContentTagError::new(open.content_start, "no closing tag"))
    }
}

#[test]
fn parser_decides_where_the_content_ends() -> Result<(), swc_ecma_parser::error::Error> {
    test_helpers::testcase_with(
        &Preprocessor::with_config(Config {
            parser: Some(Box::new(LastClosingTag(vec!["template".into()]))),
        })
        .unwrap(),
        r#"let x = <template><template id="row">hi</template></template>"#,
        r#"import { template as template_UUID } from "@ember/template-compiler";
           let x = template_UUID(`<template id="row">hi</template>`, { eval() { return eval(arguments[0])} });"#,
    )
}

#[test]
fn default_config_is_valid() {
    assert!(Preprocessor::with_config(Config::default()).is_ok());
    assert_eq!(Preprocessor::new().tags(), ["template"]);
}

#[test]
fn accepts_valid_tag_names() {
    for tag in ["template", "jsx", "my-tag", "h1", "Template"] {
        assert_eq!(configured(&[tag]).unwrap().tags(), [tag]);
    }
}

#[test]
fn rejects_invalid_tag_names() {
    for tag in [
        "",
        "1tag",
        "-tag",
        "my tag",
        "tag>",
        "<tag",
        "tag/",
        "my_tag",
        "tëmplate",
    ] {
        assert_eq!(
            configured(&[tag]).err(),
            Some(InvalidConfig::InvalidTagName(tag.to_owned())),
            "{tag:?} should be rejected"
        );
    }
}

#[test]
fn rejects_a_parser_with_no_tags() {
    assert_eq!(configured(&[]).err(), Some(InvalidConfig::NoTags));
}

#[test]
fn only_template_has_a_built_in_output() {
    let p = configured(&["template", "style"]).unwrap();
    let err = p
        .process("let s = <style>p {}</style>;", Default::default())
        .err()
        .unwrap();
    assert_eq!(
        err.error.kind(),
        &swc_ecma_parser::error::SyntaxError::ContentTag(
            "`<style>` content tags have no built-in output, so they need a transform".into()
        )
    );
    // In a class body too.
    assert!(p
        .process("class A { <style>p {}</style> }", Default::default())
        .is_err());
    // The other tag is still replaced.
    assert!(p
        .process("let t = <template>hi</template>;", Default::default())
        .unwrap()
        .code
        .contains("@ember/template-compiler"));
}

#[test]
fn a_tag_with_no_output_can_still_be_parsed() {
    let p = configured(&["template", "style"]).unwrap();
    let parsed = p
        .parse("let s = <style>p {}</style>;", Default::default())
        .unwrap();
    assert_eq!(parsed.len(), 1);
}

#[test]
fn reused_preprocessor_gives_the_same_output() {
    let p = Preprocessor::new();
    let src = "let ü = 1;\nlet x = <template>💩</template>";
    let first = p.process(src, Default::default()).unwrap();
    for _ in 0..3 {
        p.process("let y = <template>other</template>", Default::default())
            .unwrap();
        let again = p.process(src, Default::default()).unwrap();
        assert_eq!(again.code, first.code);
        assert_eq!(again.map, first.map);
    }
}

#[test]
fn reused_preprocessor_reports_errors_in_the_right_file() {
    let p = Preprocessor::new();
    p.process("let x = 1;\nlet y = 2;\n", Default::default())
        .unwrap();
    let err = p.process("let x = ;", Default::default()).err().unwrap();
    assert_eq!(
        err.source_map
            .span_to_string(swc_common::Spanned::span(&err.error)),
        "<anon>:1:9: 1:10"
    );
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    fn contents(recovered: &Recovered) -> Vec<(&str, &str)> {
        recovered
            .templates
            .iter()
            .map(|template| (template.tag_name(), template.contents()))
            .collect()
    }

    #[test]
    fn a_file_that_parses_gives_the_same_templates_as_parse() {
        let p = Preprocessor::new();
        let src = "class A { <template>a</template> }\nlet x = <template>b</template>;";
        let recovered = p.parse_with_recovery(src, Default::default());
        assert_eq!(
            recovered.templates,
            p.parse(src, Default::default()).unwrap()
        );
        assert!(recovered.errors.is_empty());
    }

    #[test]
    fn finds_templates_around_a_syntax_error() {
        let p = Preprocessor::new();
        let src =
            "const T = <template>hi</template>;\nconst a =;\nconst U = <template>yo</template>;";
        let recovered = p.parse_with_recovery(src, Default::default());
        assert_eq!(
            contents(&recovered),
            [("template", "hi"), ("template", "yo")]
        );
        // The parse error points at the `;` after `=`.
        let error = &recovered.errors[0];
        assert_eq!(error.kind, RecoveryErrorKind::Syntax);
        assert_eq!(error.range.start_byte(), src.find("=;").unwrap() + 1);
    }

    #[test]
    fn a_missing_closing_tag_is_reported_on_the_opening_tag() {
        let p = Preprocessor::new();
        let src = "let x = <template>a;\n";
        let recovered = p.parse_with_recovery(src, Default::default());
        assert!(recovered.templates.is_empty());
        let error = &recovered.errors[0];
        assert_eq!(error.kind, RecoveryErrorKind::MissingClosingTag);
        assert_eq!(error.range.start_byte(), src.find("<template>").unwrap());
    }

    #[test]
    fn recovered_templates_have_the_same_ranges_as_parsed_ones() {
        let p = Preprocessor::new();
        let valid = "let ü = '💩';\nclass A { <template>💩</template> }";
        let broken = format!("{valid}\nconst a =;");
        let parsed = p.parse(valid, Default::default()).unwrap();
        let recovered = p.parse_with_recovery(&broken, Default::default());
        assert!(!recovered.errors.is_empty());
        assert_eq!(recovered.templates, parsed);
    }

    #[test]
    fn guesses_whether_a_template_is_a_class_member() {
        let p = Preprocessor::new();
        let src = "class A { <template>member</template> }\nlet x = <template>expression</template>;\nfoo(";
        let recovered = p.parse_with_recovery(src, Default::default());
        let parsed = p
            .parse(
                "class A { <template>member</template> }\nlet x = <template>expression</template>;",
                Default::default(),
            )
            .unwrap();
        assert_eq!(recovered.templates, parsed);
    }

    #[test]
    fn ignores_templates_in_regexes_and_template_literals() {
        let p = Preprocessor::new();
        let src = "const r = /<template>no<\\/template>/;\nconst s = `${a}<template>no</template>`;\nconst T = <template>hi</template>;\nfoo(";
        let recovered = p.parse_with_recovery(src, Default::default());
        assert_eq!(contents(&recovered), [("template", "hi")]);
    }

    #[test]
    fn uses_the_parser_to_find_where_content_ends() {
        let p = Preprocessor::with_config(Config {
            parser: Some(Box::new(LastClosingTag(vec!["template".into()]))),
        })
        .unwrap();
        let src = "let x = <template><template>a</template></template>;\nconst a =;";
        let recovered = p.parse_with_recovery(src, Default::default());
        assert_eq!(
            contents(&recovered),
            [("template", "<template>a</template>")]
        );
    }

    #[test]
    fn errors_come_in_order_with_the_parse_error_first() {
        let p = Preprocessor::new();
        let src = "const a =;\nlet x = <template>no closing tag";
        let recovered = p.parse_with_recovery(src, Default::default());
        assert!(recovered.templates.is_empty());
        assert!(recovered.errors.len() >= 2, "{:?}", recovered.errors);
        assert!(recovered.errors[1..].iter().any(|error| error
            .message
            .contains("The closing `</template>` tag is missing")));
    }
}
