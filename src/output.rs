//! The code that `Preprocessor::process` writes in place of each content tag.

use std::collections::HashSet;
use std::fmt;
use swc_atoms::Atom;
use swc_common::{sync::Lrc, FileName, SourceMap, Span, Spanned, DUMMY_SP};
use swc_ecma_ast::{
    BlockStmt, ClassMember, Decl, Expr, ExprStmt, Ident, IdentName, ImportSpecifier, Invalid,
    KeyValueProp, Lit, Module, ModuleDecl, ModuleExportName, ModuleItem, Prop, PropName,
    StaticBlock, Stmt,
};
use swc_ecma_parser::{lexer::Lexer, Parser, StringInput, Syntax, TsSyntax};
use swc_ecma_visit::{Visit, VisitMut, VisitMutWith, VisitWith};

/// Marks where the content of the tag goes in `Output::expression` and
/// `Output::class_member`. How it is written decides what replaces it.
///
/// - `` `___CT_CONTENT___` `` is replaced with the content as a template
///   literal.
/// - `"___CT_CONTENT___"` or `'___CT_CONTENT___'` is replaced with the content
///   as a string literal.
/// - `___CT_CONTENT___` is replaced with the content read as JavaScript code.
pub const CONTENT_PLACEHOLDER: &str = "___CT_CONTENT___";

/// Added to each name bound by `Output::import`, so that the import cannot
/// clash with a name in the file being processed.
pub const RENAME_SUFFIX: &str = "fd9b2463e5f141cfb5666b64daa1f11a";

/// The code to write in place of each content tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Import declarations added to the top of a file that has at least one
    /// content tag. Can be empty.
    pub import: String,
    /// The expression that replaces a content tag in an expression position.
    pub expression: String,
    /// The class member that replaces a content tag in a class body. When it
    /// is `None`, the tag is replaced with `static { <expression>; }`. When it
    /// is empty, the tag is removed.
    pub class_member: Option<String>,
    /// Remove the indentation that all lines of the content share.
    pub strip_indent: bool,
    /// Turn a content tag that is a whole top-level statement into
    /// `export default <expression>`.
    pub export_default: bool,
}

impl Output {
    /// The output for Ember, which compiles the content with
    /// `@ember/template-compiler`.
    pub fn ember() -> Self {
        Self {
            import: r#"import { template } from "@ember/template-compiler";"#.into(),
            expression:
                "template(`___CT_CONTENT___`, { eval() { return eval(arguments[0]); } })".into(),
            class_member: Some(
                "static { template(`___CT_CONTENT___`, { component: this, eval() { return eval(arguments[0]); } }); }"
                    .into(),
            ),
            strip_indent: true,
            export_default: true,
        }
    }
}

impl Default for Output {
    fn default() -> Self {
        Self::ember()
    }
}

/// One of the `Output` snippets is not allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidOutput {
    /// The name of the JavaScript option, such as `classMember`.
    pub option: &'static str,
    pub reason: String,
}

impl fmt::Display for InvalidOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid `output.{}` option. {}",
            self.option, self.reason
        )
    }
}

impl std::error::Error for InvalidOutput {}

fn invalid(option: &'static str, reason: impl Into<String>) -> InvalidOutput {
    InvalidOutput {
        option,
        reason: reason.into(),
    }
}

/// A parsed expression or class member snippet.
#[derive(Clone)]
struct Snippet<T> {
    node: T,
    // From the start of the first placeholder to the end of the last one, in
    // the source of the snippet. `None` when the snippet has no placeholder.
    placeholders: Option<Span>,
    // The snippet uses the placeholder form that is replaced with code.
    uses_code: bool,
}

/// An `Output` whose snippets have been parsed and checked.
#[derive(Clone)]
pub(crate) struct CompiledOutput {
    pub imports: Vec<ModuleItem>,
    expression: Snippet<Box<Expr>>,
    // `None` when content tags in class bodies are removed.
    class_member: Option<Snippet<ClassMember>>,
    pub strip_indent: bool,
    pub export_default: bool,
}

impl CompiledOutput {
    pub fn new(output: &Output) -> Result<Self, InvalidOutput> {
        Self::compile(output, true)
    }

    /// Keeps the imported names as they are written, so tests can compare
    /// against readable output.
    #[cfg(test)]
    pub fn without_renaming(output: &Output) -> Result<Self, InvalidOutput> {
        Self::compile(output, false)
    }

    fn compile(output: &Output, rename: bool) -> Result<Self, InvalidOutput> {
        let mut imports = parse_imports(&output.import)?;
        let mut expression = parse_expression(&output.expression)?;
        let mut class_member = match &output.class_member {
            Some(src) if src.trim().is_empty() => None,
            Some(src) => Some(parse_class_member(src)?),
            None => Some(Snippet {
                node: ClassMember::StaticBlock(StaticBlock {
                    span: DUMMY_SP,
                    body: BlockStmt {
                        stmts: vec![Stmt::Expr(ExprStmt {
                            span: DUMMY_SP,
                            expr: expression.node.clone(),
                        })],
                        ..Default::default()
                    },
                }),
                placeholders: expression.placeholders,
                uses_code: expression.uses_code,
            }),
        };

        if rename {
            let names = rename_imports(&mut imports);
            let mut renamer = Renamer { names };
            expression.node.visit_mut_with(&mut renamer);
            if let Some(class_member) = &mut class_member {
                class_member.node.visit_mut_with(&mut renamer);
            }
        }

        // The snippets were parsed from their own source map, so their spans
        // would point at the wrong places in the processed file.
        imports.visit_mut_with(&mut ClearSpans);

        Ok(Self {
            imports,
            expression,
            class_member,
            strip_indent: output.strip_indent,
            export_default: output.export_default,
        })
    }

    /// The expression snippet replaces a placeholder with code.
    pub fn expression_uses_code(&self) -> bool {
        self.expression.uses_code
    }

    /// The class member snippet replaces a placeholder with code.
    pub fn class_member_uses_code(&self) -> bool {
        self.class_member
            .as_ref()
            .is_some_and(|class_member| class_member.uses_code)
    }

    /// The expression that replaces a content tag.
    pub fn expression(&self, tag: TagSpans, content: &Content) -> Expr {
        let mut expr = (*self.expression.node).clone();
        fill(&mut expr, self.expression.placeholders, tag, content);
        expr
    }

    /// The class member that replaces a content tag, or `None` when the tag is
    /// removed.
    pub fn class_member(&self, tag: TagSpans, content: &Content) -> Option<ClassMember> {
        let class_member = self.class_member.as_ref()?;
        let mut member = class_member.node.clone();
        fill(&mut member, class_member.placeholders, tag, content);
        Some(member)
    }
}

/// Where a content tag and its opening and closing tags are in the processed
/// file.
#[derive(Clone, Copy)]
pub(crate) struct TagSpans {
    pub whole: Span,
    pub opening: Span,
    pub closing: Span,
}

/// The content of a tag in each form a placeholder can ask for.
pub(crate) struct Content {
    pub template: Expr,
    pub string: Expr,
    // Only parsed when a snippet uses the code form.
    pub code: Option<Expr>,
}

/// How a placeholder is written, which decides what replaces it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlaceholderForm {
    Template,
    String,
    Code,
}

fn placeholder_form(expr: &Expr) -> Option<PlaceholderForm> {
    match expr {
        Expr::Ident(ident) if &*ident.sym == CONTENT_PLACEHOLDER => Some(PlaceholderForm::Code),
        Expr::Lit(Lit::Str(str))
            if str
                .value
                .as_atom()
                .is_some_and(|value| &**value == CONTENT_PLACEHOLDER) =>
        {
            Some(PlaceholderForm::String)
        }
        Expr::Tpl(tpl)
            if tpl.exprs.is_empty()
                && tpl.quasis.len() == 1
                && &*tpl.quasis[0].raw == CONTENT_PLACEHOLDER =>
        {
            Some(PlaceholderForm::Template)
        }
        _ => None,
    }
}

fn fill<N: VisitMutWith<SpanReplacer> + for<'a> VisitMutWith<PlaceholderReplacer<'a>>>(
    node: &mut N,
    placeholders: Option<Span>,
    tag: TagSpans,
    content: &Content,
) {
    node.visit_mut_with(&mut SpanReplacer { placeholders, tag });
    node.visit_mut_with(&mut PlaceholderReplacer { content });
}

fn parse_module(option: &'static str, src: &str) -> Result<Module, InvalidOutput> {
    let source_map: Lrc<SourceMap> = Default::default();
    let source_file = source_map.new_source_file(FileName::Anon.into(), src.to_string());
    let lexer = Lexer::new(
        Syntax::Typescript(TsSyntax {
            decorators: true,
            ..Default::default()
        }),
        Default::default(),
        StringInput::from(&*source_file),
        None,
    );
    let mut parser = Parser::new_from(lexer);
    let module = parser.parse_module().map_err(|err| {
        invalid(
            option,
            format!("It could not be parsed: {}.", err.kind().msg()),
        )
    })?;
    if let Some(err) = parser.take_errors().first() {
        return Err(invalid(
            option,
            format!("It could not be parsed: {}.", err.kind().msg()),
        ));
    }
    Ok(module)
}

fn parse_imports(src: &str) -> Result<Vec<ModuleItem>, InvalidOutput> {
    if src.contains(CONTENT_PLACEHOLDER) {
        return Err(invalid(
            "import",
            format!("It must not contain {CONTENT_PLACEHOLDER}."),
        ));
    }
    let module = parse_module("import", src)?;
    for item in &module.body {
        if !matches!(item, ModuleItem::ModuleDecl(ModuleDecl::Import(_))) {
            return Err(invalid(
                "import",
                "It must contain only import declarations.",
            ));
        }
    }
    Ok(module.body)
}

fn parse_expression(src: &str) -> Result<Snippet<Box<Expr>>, InvalidOutput> {
    let src = src.trim_end();
    let src = src.strip_suffix(';').unwrap_or(src);
    // The parentheses make `{` start an object, not a block. The new lines
    // stop a trailing `//` comment from hiding the closing parenthesis.
    let module = parse_module("expression", &format!("(\n{src}\n)"))?;
    let expr = match module.body.as_slice() {
        [ModuleItem::Stmt(Stmt::Expr(ExprStmt {
            expr: box Expr::Paren(paren),
            ..
        }))] => paren.expr.clone(),
        _ => {
            return Err(invalid(
                "expression",
                "It must be a single JavaScript expression.",
            ))
        }
    };
    let (placeholders, uses_code) = find_placeholders("expression", &*expr)?;
    Ok(Snippet {
        node: expr,
        placeholders,
        uses_code,
    })
}

fn parse_class_member(src: &str) -> Result<Snippet<ClassMember>, InvalidOutput> {
    let module = parse_module("classMember", &format!("class X {{\n{src}\n}}"))?;
    let mut members = match module.body.as_slice() {
        [ModuleItem::Stmt(Stmt::Decl(Decl::Class(class)))] => class.class.body.clone(),
        _ => vec![],
    };
    members.retain(|member| !matches!(member, ClassMember::Empty(_)));
    let member = match <[ClassMember; 1]>::try_from(members) {
        Ok([member]) => member,
        Err(_) => return Err(invalid("classMember", "It must be a single class member.")),
    };
    let (placeholders, uses_code) = find_placeholders("classMember", &member)?;
    Ok(Snippet {
        node: member,
        placeholders,
        uses_code,
    })
}

/// Returns where the placeholders in `node` are, and whether any of them is
/// replaced with code. A bare placeholder must be used as an expression,
/// because that is the only place code can go.
fn find_placeholders<N: VisitWith<PlaceholderFinder>>(
    option: &'static str,
    node: &N,
) -> Result<(Option<Span>, bool), InvalidOutput> {
    let mut finder = PlaceholderFinder::default();
    node.visit_with(&mut finder);
    if finder.identifiers != finder.code {
        return Err(invalid(
            option,
            format!("{CONTENT_PLACEHOLDER} can only be used as an expression."),
        ));
    }
    Ok((finder.span, finder.code > 0))
}

#[derive(Default)]
struct PlaceholderFinder {
    // Every identifier named like the placeholder, in any position.
    identifiers: usize,
    // Placeholders in the code form.
    code: usize,
    span: Option<Span>,
}

impl Visit for PlaceholderFinder {
    fn visit_ident(&mut self, n: &Ident) {
        if &*n.sym == CONTENT_PLACEHOLDER {
            self.identifiers += 1;
        }
    }

    fn visit_expr(&mut self, n: &Expr) {
        if let Some(form) = placeholder_form(n) {
            if form == PlaceholderForm::Code {
                self.code += 1;
            }
            let span = n.span();
            self.span = Some(match self.span {
                Some(found) => found
                    .with_lo(found.lo.min(span.lo))
                    .with_hi(found.hi.max(span.hi)),
                None => span,
            });
        }
        n.visit_children_with(self);
    }
}

fn renamed(sym: &Atom) -> Atom {
    format!("{sym}_{RENAME_SUFFIX}").into()
}

/// Renames the names that `imports` bind, and returns the original names.
fn rename_imports(imports: &mut [ModuleItem]) -> HashSet<Atom> {
    let mut names = HashSet::new();
    for item in imports {
        let ModuleItem::ModuleDecl(ModuleDecl::Import(import)) = item else {
            continue;
        };
        for specifier in &mut import.specifiers {
            let local = match specifier {
                ImportSpecifier::Named(named) => {
                    // `import { a }` becomes `import { a as a_<suffix> }`.
                    if named.imported.is_none() {
                        named.imported = Some(ModuleExportName::Ident(named.local.clone()));
                    }
                    &mut named.local
                }
                ImportSpecifier::Default(default) => &mut default.local,
                ImportSpecifier::Namespace(namespace) => &mut namespace.local,
            };
            names.insert(local.sym.clone());
            local.sym = renamed(&local.sym);
        }
    }
    names
}

/// Renames uses of the imported names in the expression and class member
/// snippets.
struct Renamer {
    names: HashSet<Atom>,
}

impl VisitMut for Renamer {
    fn visit_mut_ident(&mut self, n: &mut Ident) {
        if self.names.contains(&n.sym) {
            n.sym = renamed(&n.sym);
        }
    }

    fn visit_mut_prop(&mut self, n: &mut Prop) {
        // `{ a }` must become `{ a: a_<suffix> }`, not `{ a_<suffix> }`.
        if let Prop::Shorthand(ident) = n {
            if self.names.contains(&ident.sym) {
                *n = Prop::KeyValue(KeyValueProp {
                    key: PropName::Ident(IdentName::new(ident.sym.clone(), ident.span)),
                    value: Box::new(Expr::Ident(Ident {
                        sym: renamed(&ident.sym),
                        ..ident.clone()
                    })),
                });
                return;
            }
        }
        n.visit_mut_children_with(self);
    }
}

struct ClearSpans;

impl VisitMut for ClearSpans {
    fn visit_mut_span(&mut self, n: &mut Span) {
        *n = DUMMY_SP;
    }
}

/// Points the spans of a snippet at the content tag it replaces, so source
/// maps lead back to the tag. Code written before the placeholders maps to the
/// opening tag, code after them maps to the closing tag, and everything else
/// maps to the whole tag.
struct SpanReplacer {
    placeholders: Option<Span>,
    tag: TagSpans,
}

impl VisitMut for SpanReplacer {
    fn visit_mut_span(&mut self, n: &mut Span) {
        *n = match self.placeholders {
            _ if n.is_dummy() => self.tag.whole,
            Some(placeholders) if n.hi <= placeholders.lo => self.tag.opening,
            Some(placeholders) if n.lo >= placeholders.hi => self.tag.closing,
            _ => self.tag.whole,
        };
    }
}

struct PlaceholderReplacer<'a> {
    content: &'a Content,
}

impl VisitMut for PlaceholderReplacer<'_> {
    fn visit_mut_expr(&mut self, n: &mut Expr) {
        match placeholder_form(n) {
            Some(PlaceholderForm::Template) => *n = self.content.template.clone(),
            Some(PlaceholderForm::String) => *n = self.content.string.clone(),
            Some(PlaceholderForm::Code) => {
                *n = self.content.code.clone().unwrap_or_else(|| {
                    // The transform parses the code whenever a snippet uses
                    // this form, so this only happens if that code failed to
                    // parse, and that error is reported instead.
                    Expr::Invalid(Invalid { span: n.span() })
                })
            }
            None => n.visit_mut_children_with(self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(import: &str, expression: &str, class_member: Option<&str>) -> Output {
        Output {
            import: import.into(),
            expression: expression.into(),
            class_member: class_member.map(Into::into),
            strip_indent: true,
            export_default: true,
        }
    }

    fn error(output: &Output) -> InvalidOutput {
        match CompiledOutput::new(output) {
            Ok(_) => panic!("expected {output:?} to be rejected"),
            Err(err) => err,
        }
    }

    // The content as each placeholder form would receive it.
    fn content(span: Span) -> Content {
        use swc_ecma_ast::Str;
        let string = |text: &str| {
            Expr::Lit(Lit::Str(Str {
                span,
                value: text.into(),
                raw: None,
            }))
        };
        Content {
            template: string("template"),
            string: string("string"),
            code: Some(string("code")),
        }
    }

    // The forms used by `expression`, as the text that replaced each one.
    fn forms_used(expression: &str) -> Vec<String> {
        use swc_ecma_ast::{ArrayLit, Str};
        let compiled = CompiledOutput::new(&output("", expression, None)).unwrap();
        let tag = TagSpans {
            whole: DUMMY_SP,
            opening: DUMMY_SP,
            closing: DUMMY_SP,
        };
        let Expr::Array(ArrayLit { elems, .. }) = compiled.expression(tag, &content(DUMMY_SP))
        else {
            panic!("expected an array");
        };
        elems
            .into_iter()
            .map(|elem| match elem.map(|elem| *elem.expr) {
                Some(Expr::Lit(Lit::Str(Str { value, .. }))) => {
                    value.as_atom().unwrap().to_string()
                }
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn the_placeholder_form_decides_what_replaces_it() {
        assert_eq!(
            forms_used(
                "[`___CT_CONTENT___`, \"___CT_CONTENT___\", '___CT_CONTENT___', ___CT_CONTENT___]"
            ),
            ["template", "string", "string", "code"]
        );
    }

    #[test]
    fn literals_with_other_text_are_not_placeholders() {
        assert_eq!(
            forms_used("[`a ___CT_CONTENT___`, \"___CT_CONTENT___ b\", `${x}___CT_CONTENT___`]")
                .len(),
            3
        );
        let compiled = CompiledOutput::new(&output(
            "",
            "[`a ___CT_CONTENT___`, \"___CT_CONTENT___ b\"]",
            None,
        ))
        .unwrap();
        assert!(compiled.expression.placeholders.is_none());
    }

    #[test]
    fn only_the_bare_form_needs_the_content_as_code() {
        let uses_code = |expression| {
            CompiledOutput::new(&output(
                "",
                expression,
                Some(expression_member(expression).as_str()),
            ))
            .unwrap()
            .expression_uses_code()
        };
        assert!(!uses_code("c(`___CT_CONTENT___`)"));
        assert!(!uses_code("c(\"___CT_CONTENT___\")"));
        assert!(uses_code("c(___CT_CONTENT___)"));
    }

    fn expression_member(expression: &str) -> String {
        format!("static x = {expression};")
    }

    #[test]
    fn an_empty_class_member_removes_the_tag() {
        let compiled =
            CompiledOutput::new(&output("", "c(`___CT_CONTENT___`)", Some("  "))).unwrap();
        let tag = TagSpans {
            whole: DUMMY_SP,
            opening: DUMMY_SP,
            closing: DUMMY_SP,
        };
        assert!(compiled.class_member(tag, &content(DUMMY_SP)).is_none());
        assert!(!compiled.class_member_uses_code());
    }

    #[test]
    fn a_missing_class_member_uses_the_expression() {
        let compiled = CompiledOutput::new(&output("", "c(___CT_CONTENT___)", None)).unwrap();
        assert!(compiled.class_member_uses_code());
    }

    #[test]
    fn spans_point_at_the_opening_tag_closing_tag_or_whole_tag() {
        use swc_common::BytePos;
        use swc_ecma_ast::Callee;

        let span = |lo, hi| Span::new(BytePos(lo), BytePos(hi));
        let tag = TagSpans {
            whole: span(1, 30),
            opening: span(1, 11),
            closing: span(19, 30),
        };
        let compiled = CompiledOutput::without_renaming(&Output::ember()).unwrap();
        let Expr::Call(call) = compiled.expression(tag, &content(span(11, 19))) else {
            panic!("expected a call expression");
        };

        // The call contains the placeholder, so it maps to the whole tag.
        assert_eq!(call.span, tag.whole);
        // `template` is written before the placeholder.
        let Callee::Expr(box Expr::Ident(callee)) = &call.callee else {
            panic!("expected an identifier callee");
        };
        assert_eq!(callee.span, tag.opening);
        // The content keeps its own span.
        assert_eq!(call.args[0].expr.span(), span(11, 19));
        // The `{ eval() ... }` object is written after the placeholder.
        assert_eq!(call.args[1].expr.span(), tag.closing);
    }

    #[test]
    fn spans_point_at_the_whole_tag_without_a_placeholder() {
        use swc_common::BytePos;

        let span = |lo, hi| Span::new(BytePos(lo), BytePos(hi));
        let tag = TagSpans {
            whole: span(1, 30),
            opening: span(1, 11),
            closing: span(19, 30),
        };
        let compiled = CompiledOutput::new(&output("", "c()", None)).unwrap();
        let Expr::Call(call) = compiled.expression(tag, &content(span(11, 19))) else {
            panic!("expected a call expression");
        };
        assert_eq!(call.span, tag.whole);
    }

    #[test]
    fn ember_output_is_valid() {
        assert!(CompiledOutput::new(&Output::ember()).is_ok());
    }

    #[test]
    fn accepts_an_empty_import() {
        let compiled = CompiledOutput::new(&output("", "c(___CT_CONTENT___)", None)).unwrap();
        assert!(compiled.imports.is_empty());
    }

    #[test]
    fn accepts_several_imports() {
        let compiled = CompiledOutput::new(&output(
            r#"import a from "a"; import { b } from "b";"#,
            "a(b(___CT_CONTENT___))",
            None,
        ))
        .unwrap();
        assert_eq!(compiled.imports.len(), 2);
    }

    #[test]
    fn accepts_a_trailing_semicolon_in_the_expression() {
        assert!(CompiledOutput::new(&output("", "c(___CT_CONTENT___);", None)).is_ok());
    }

    #[test]
    fn accepts_any_number_of_placeholders() {
        for expression in [
            "a()",
            "a(`___CT_CONTENT___`, `___CT_CONTENT___`)",
            "a(___CT_CONTENT___, ___CT_CONTENT___)",
        ] {
            assert!(
                CompiledOutput::new(&output(
                    "",
                    expression,
                    Some(&expression_member(expression))
                ))
                .is_ok(),
                "{expression} should be accepted"
            );
        }
    }

    #[test]
    fn rejects_statements_in_the_import() {
        let err = error(&output(
            r#"import a from "a"; a();"#,
            "a(___CT_CONTENT___)",
            None,
        ));
        assert_eq!(err.option, "import");
    }

    #[test]
    fn rejects_the_placeholder_in_the_import() {
        let err = error(&output(
            r#"import ___CT_CONTENT___ from "a";"#,
            "a(___CT_CONTENT___)",
            None,
        ));
        assert_eq!(err.option, "import");
    }

    #[test]
    fn rejects_an_import_that_does_not_parse() {
        let err = error(&output("import from", "a(___CT_CONTENT___)", None));
        assert_eq!(err.option, "import");
    }

    #[test]
    fn rejects_more_than_one_expression() {
        let err = error(&output("", "a(___CT_CONTENT___)); b(", None));
        assert_eq!(err.option, "expression");
    }

    #[test]
    fn rejects_the_placeholder_as_a_binding() {
        let err = error(&output("", "(___CT_CONTENT___) => ___CT_CONTENT___", None));
        assert_eq!(err.option, "expression");
    }

    #[test]
    fn rejects_more_than_one_class_member() {
        let err = error(&output(
            "",
            "a(___CT_CONTENT___)",
            Some("static a = ___CT_CONTENT___; static b = 1;"),
        ));
        assert_eq!(err.option, "classMember");
    }

    #[test]
    fn property_names_are_not_placeholders() {
        let compiled = CompiledOutput::new(&output(
            "",
            "a.___CT_CONTENT___",
            Some("static ___CT_CONTENT___ = 1;"),
        ))
        .unwrap();
        assert!(compiled.expression.placeholders.is_none());
        assert!(!compiled.class_member_uses_code());
    }

    #[test]
    fn error_message_names_the_option() {
        let err = error(&output("", "(___CT_CONTENT___) => 1", None));
        assert_eq!(
            err.to_string(),
            "Invalid `output.expression` option. ___CT_CONTENT___ can only be used as an expression."
        );
    }
}
