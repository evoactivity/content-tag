use swc_common::{BytePos, Span, Spanned};
use swc_core::ecma::{
    ast::{
        BlockStmt, CallExpr, Callee, ClassMember, ContentTagExpression, ContentTagMember, Expr,
        ExprStmt, Ident, StaticBlock, Stmt,
    },
    visit::VisitMut,
    visit::VisitMutWith,
};

#[cfg(test)]
use swc_ecma_ast::Pass;
use swc_ecma_ast::{
    ContentTagContent, ExportDefaultExpr, ExprOrSpread, Invalid, ModuleDecl, ModuleItem, Tpl,
    TplElement, TsSatisfiesExpr, TsType,
};
use swc_ecma_parser::error::{Error, SyntaxError};

use swc_atoms::Atom;

/// The one content tag that has a built-in output.
const EMBER_TAG: &str = "template";

pub struct TransformVisitor<'a> {
    template_identifier: Ident,
    found_it: Option<&'a mut bool>,
    // The source and where it starts, to read each content tag's name.
    source: &'a str,
    start_pos: BytePos,
    /// The first error, from a content tag that has no built-in output.
    pub error: Option<Error>,
}

impl<'a> TransformVisitor<'a> {
    pub fn new(
        id: &Ident,
        found_it: Option<&'a mut bool>,
        source: &'a str,
        start_pos: BytePos,
    ) -> Self {
        TransformVisitor {
            template_identifier: id.clone(),
            found_it,
            source,
            start_pos,
            error: None,
        }
    }

    // The name of the content tag whose opening tag is at `opening`. The
    // syntax tree does not record it, so it is read from the source.
    fn tag_name(&self, opening: Span) -> &'a str {
        let start = (opening.lo - self.start_pos).0 as usize;
        let end = (opening.hi - self.start_pos).0 as usize;
        &self.source[start + 1..end - 1]
    }

    // Whether the content tag at `opening` has the built-in output. Records
    // an error when it does not.
    fn has_output(&mut self, opening: Span) -> bool {
        let name = self.tag_name(opening);
        if name == EMBER_TAG {
            return true;
        }
        self.error.get_or_insert_with(|| {
            Error::new(
                opening,
                SyntaxError::ContentTag(format!(
                    "`<{name}>` content tags have no built-in output, so they need a transform"
                )),
            )
        });
        false
    }
    fn set_found_it(&mut self) {
        match self.found_it.as_mut() {
            Some(flag) => **flag = true,
            None => {}
        }
    }
    fn transform_tag_expression(&mut self, expr: &ContentTagExpression) -> Expr {
        let ContentTagExpression {
            span,
            opening,
            contents,
            closing,
        } = expr;
        if !self.has_output(opening.span) {
            return Expr::Invalid(Invalid { span: *span });
        }

        Expr::Call(CallExpr {
            span: *span,
            callee: Callee::Expr(Box::new(Expr::Ident(self.template_identifier.clone()))),
            args: vec![
                self.content_literal(contents),
                crate::snippets::scope_params(closing.span).into(),
            ],
            ..Default::default()
        })
    }

    fn content_literal(&self, contents: &Box<ContentTagContent>) -> ExprOrSpread {
        let stripped_content = strip_indent(&contents.value);
        Box::new(Expr::Tpl(Tpl {
            span: contents.span,
            exprs: vec![],
            quasis: vec![TplElement {
                span: contents.span,
                cooked: None,
                raw: escape_template_literal(&stripped_content.into()),
                tail: false,
            }],
        }))
        .into()
    }
}

fn escape_template_literal(input: &Atom) -> Atom {
    input
        .replace("\\", "\\\\")
        .replace("`", "\\`")
        .replace("$", "\\$")
        .into()
}

// The leading spaces and tabs of a line. Only ASCII whitespace counts as
// indentation, so slicing a line at (up to) this length never splits a
// multi-byte character.
fn indentation(line: &str) -> &str {
    let content = line.trim_start_matches([' ', '\t']);
    &line[..line.len() - content.len()]
}

// A line made up only of indentation. Lines with any other character, including
// non-ASCII whitespace, are content.
fn is_blank(line: &str) -> bool {
    indentation(line).len() == line.len()
}

fn strip_indent(input: &str) -> String {
    let lines: Vec<&str> = input.lines().collect();

    if lines.len() <= 1 {
        return input.to_string();
    }

    let start = lines
        .iter()
        .position(|l| !is_blank(l))
        .unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|l| !is_blank(l))
        .map(|i| i + 1)
        .unwrap_or(0);

    if start >= end {
        return String::new();
    }

    let lines = &lines[start..end];

    let mut min_indent: Option<usize> = None;
    let mut has_spaces = false;
    let mut has_tabs = false;

    for line in lines {
        if is_blank(line) {
            continue;
        }

        let indent_chars = indentation(line);
        let indent_size = indent_chars.len();

        has_spaces |= indent_chars.contains(' ');
        has_tabs |= indent_chars.contains('\t');

        if has_spaces && has_tabs {
            return lines.join("\n");
        }

        min_indent = Some(min_indent.map_or(indent_size, |current| current.min(indent_size)));
    }

    let min_indent = min_indent.unwrap_or(0);

    if min_indent == 0 {
        return lines.join("\n");
    }

    lines
        .iter()
        .map(|line| {
            if line.len() >= min_indent {
                &line[min_indent..]
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl<'a> VisitMut for TransformVisitor<'a> {
    fn visit_mut_expr(&mut self, n: &mut Expr) {
        n.visit_mut_children_with(self);
        if let Expr::ContentTagExpression(expr) = n {
            *n = self.transform_tag_expression(expr);
            self.set_found_it();
        }
    }

    fn visit_mut_class_member(&mut self, n: &mut ClassMember) {
        n.visit_mut_children_with(self);
        if let ClassMember::ContentTagMember(ContentTagMember {
            span,
            opening,
            contents,
            closing,
        }) = n
        {
            if !self.has_output(opening.span) {
                return;
            }
            let call_expr = Expr::Call(CallExpr {
                span: *span,
                callee: Callee::Expr(Box::new(Expr::Ident(self.template_identifier.clone()))),
                args: vec![
                    self.content_literal(contents),
                    crate::snippets::scope_params_with_this(closing.span).into(),
                ],
                ..Default::default()
            });
            let call_statement = ExprStmt {
                span: *span,
                expr: Box::new(call_expr),
            };
            *n = ClassMember::StaticBlock(StaticBlock {
                span: opening.span,
                body: BlockStmt {
                    span: *span,
                    stmts: vec![Stmt::Expr(call_statement)],
                    ..Default::default()
                },
            });
            self.set_found_it();
        }
    }

    fn visit_mut_module_items(&mut self, items: &mut Vec<ModuleItem>) {
        let mut items_updated = Vec::with_capacity(items.len());
        for item in items.drain(..) {
            if let Some(content_tag) = content_tag_expression_statement(&item) {
                items_updated.push(ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(
                    ExportDefaultExpr {
                        span: content_tag.span,
                        expr: Box::new(self.transform_tag_expression(&content_tag)),
                    },
                )));
                self.set_found_it();
            } else if let Some(satisfies) = content_tag_satisfies_expression_statement(&item) {
                items_updated.push(ModuleItem::ModuleDecl(ModuleDecl::ExportDefaultExpr(
                    ExportDefaultExpr {
                        span: satisfies.template.span,
                        expr: Box::new(Expr::TsSatisfies(TsSatisfiesExpr {
                            expr: Box::new(self.transform_tag_expression(&satisfies.template)),
                            type_ann: satisfies.ts_type.clone(),
                            span: satisfies.ts_type.span(),
                        })),
                    },
                )));
                self.set_found_it();
            } else {
                items_updated.push(item);
            }
        }

        *items = items_updated;
        items.visit_mut_children_with(self)
    }
}

fn content_tag_expression_statement(item: &ModuleItem) -> Option<&ContentTagExpression> {
    if let ModuleItem::Stmt(Stmt::Expr(ExprStmt {
        expr: box Expr::ContentTagExpression(content_tag),
        ..
    })) = item
    {
        Some(content_tag)
    } else {
        None
    }
}

struct TemplateSatisfies<'a> {
    pub template: &'a ContentTagExpression,
    pub ts_type: &'a Box<TsType>,
}

fn content_tag_satisfies_expression_statement(item: &ModuleItem) -> Option<TemplateSatisfies<'_>> {
    if let ModuleItem::Stmt(Stmt::Expr(ExprStmt {
        expr:
            box Expr::TsSatisfies(TsSatisfiesExpr {
                expr: box Expr::ContentTagExpression(content_tag),
                type_ann: ts_type,
                ..
            }),
        ..
    })) = item
    {
        Some(TemplateSatisfies {
            template: content_tag,
            ts_type,
        })
    } else {
        None
    }
}

#[cfg(test)]
impl<'a> TransformVisitor<'a> {
    pub fn into_pass(self) -> impl Pass + use<'a> {
        use swc_ecma_visit::visit_mut_pass;

        visit_mut_pass(self)
    }
}

// swc's `test_inline_input_output` parses with a lexer that has no content tag
// name, so it would not read any content tags. This parses the input with the
// tag name set, then compares the output the same way.
#[cfg(test)]
fn test_transform(input: &str, expected: &str) {
    use swc_common::FileName;
    use swc_core::ecma::transforms::{base::fixer::fixer, base::hygiene::hygiene, testing::Tester};
    use swc_ecma_ast::{noop_pass, Program};
    use swc_ecma_parser::{lexer::Lexer, Parser, StringInput, Syntax};

    let id = Ident::new_no_ctxt("template".into(), Default::default());
    let parser = crate::TagNames::new(["template"]);
    let (actual, expected) = Tester::run(|tester| {
        let source_file = tester
            .cm
            .new_source_file(FileName::Real("input.js".into()).into(), input.to_string());
        let lexer = Lexer::new(
            Syntax::default(),
            Default::default(),
            StringInput::from(&*source_file),
            Some(&tester.comments),
        )
        .with_content_tag_parser(&parser);
        let module = Parser::new_from(lexer)
            .parse_module()
            .map_err(|err| err.into_diagnostic(tester.handler).emit())?;
        let actual = Program::Module(module)
            .apply(
                TransformVisitor::new(&id, None, &source_file.src, source_file.start_pos)
                    .into_pass(),
            )
            .apply(hygiene())
            .apply(fixer(Some(&tester.comments)));
        let actual = tester.print(&actual, &Default::default());

        let expected = tester.apply_transform(
            noop_pass(),
            "expected.js",
            Syntax::default(),
            Some(true),
            expected,
        )?;
        let expected = tester.print(&expected, &Default::default());

        Ok((actual, expected))
    });

    assert_eq!(actual, expected, "\nInput:\n{input}");
}

macro_rules! test {
    ($test_name:ident, $input:expr, $expected:expr) => {
        #[test]
        fn $test_name() {
            test_transform($input, $expected)
        }
    };
}

test!(
    content_tag_template_expression,
    r#"let x = <template>Hello</template>"#,
    r#"let x = template(`Hello`, { eval() { return eval(arguments[0]); }})"#
);

test!(
    content_tag_template_member,
    r#"class X { <template>Hello</template> } "#,
    r#"class X {
      static {
          template(`Hello`, { component: this, eval() { return eval(arguments[0]) }},);
      }
  }"#
);

test!(
    expression_inside_class_member,
    r#"class X { thing = <template>Hello</template> } "#,
    r#"class X {
        thing = template(`Hello`, { eval() { return eval(arguments[0]) }},);
    }"#
);

test!(
    class_member_inside_expression,
    r#"let x = class { <template>Hello</template> } "#,
    r#"let x = class {
        static {
            template(`Hello`, { component: this, eval() { return eval(arguments[0]) }},);
        }
    }"#
);

test!(
    content_tag_export_default,
    r#"<template>Hello</template>"#,
    r#"export default template(`Hello`, { eval() { return eval(arguments[0]) }},);"#
);

test!(
    inner_expression,
    r#"let x = doIt(<template>Hello</template>)"#,
    r#"let x = doIt(template(`Hello`, { eval() { return eval(arguments[0]) }}))"#
);

test!(
    backtick_in_template,
    r#"let x = <template>He`llo</template>"#,
    r#"let x = template(`He\`llo`, { eval() { return eval(arguments[0]) }})"#
);

test!(
    dollar_in_template,
    r#"let x = <template>He${ll}o</template>"#,
    r#"let x = template(`He\${ll}o`, { eval() { return eval(arguments[0]) }})"#
);

test!(
    do_not_interpret_js_escapes_in_hbs,
    r#"let x = <template>Hello\nWorld\u1234</template>"#,
    r#"let x = template(`Hello\\nWorld\\u1234`, { eval() { return eval(arguments[0]) }})"#
);

test!(
    strips_leading_trailing_whitespace,
    r#"let x = <template>
  <span>Hello</span>
</template>"#,
    r#"let x = template(`<span>Hello</span>`, { eval() { return eval(arguments[0]) }})"#
);

test!(
    strips_common_indentation,
    r#"let x = <template>
    <div>
      <span>Hello</span>
    </div>
  </template>"#,
    r#"let x = template(`<div>
  <span>Hello</span>
</div>`, { eval() { return eval(arguments[0]) }})"#
);

test!(
    strips_indentation_multiline,
    r#"<template>
  Hello
  <span>there</span>.
  <p>
    <span>how are you</span>
  </p>
</template>"#,
    r#"export default template(`Hello
<span>there</span>.
<p>
  <span>how are you</span>
</p>`, { eval() { return eval(arguments[0]) }},);"#
);

test!(
    class_member_strips_indentation,
    r#"class X {
  <template>
    <span>Hello</span>
  </template>
}"#,
    r#"class X {
  static {
      template(`<span>Hello</span>`, { component: this, eval() { return eval(arguments[0]) }},);
  }
}"#
);

test!(
    deeply_nested_indentation,
    r#"class Component {
  method() {
    return <template>
      <div>
        <span>Nested</span>
      </div>
    </template>;
  }
}"#,
    r#"class Component {
  method() {
    return template(`<div>
  <span>Nested</span>
</div>`, { eval() { return eval(arguments[0]) }});
  }
}"#
);

test!(
    preserves_internal_indentation,
    r#"let x = <template>
  <div>
    <pre>
      some code
        with indentation
    </pre>
  </div>
</template>"#,
    r#"let x = template(`<div>
  <pre>
    some code
      with indentation
  </pre>
</div>`, { eval() { return eval(arguments[0]) }})"#
);

test!(
    opt_out_with_comment,
    r#"let x = <template>
{{!-- prevent automatic de-indent --}}
    <pre>
      content here
    </pre>
  </template>"#,
    r#"let x = template(`{{!-- prevent automatic de-indent --}}
    <pre>
      content here
    </pre>`, { eval() { return eval(arguments[0]) }})"#
);

// Only ASCII spaces and tabs count as indentation. Other Unicode whitespace is
// content, and must never be split in the middle of a multi-byte character.
test!(
    non_ascii_whitespace_is_not_indentation,
    "let x = <template>\n\u{3000}<a></a>\n  <b></b>\n</template>",
    "let x = template(`\u{3000}<a></a>\n  <b></b>`, { eval() { return eval(arguments[0]) }})"
);

test!(
    multibyte_whitespace_on_blank_line,
    "let x = <template>\n  <a></a>\n\u{3000}\n  <b></b>\n</template>",
    "let x = template(`  <a></a>\n\u{3000}\n  <b></b>`, { eval() { return eval(arguments[0]) }})"
);
