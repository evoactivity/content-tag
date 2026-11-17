use serde::Serialize;
use swc_common::{self, BytePos, Span};
use swc_ecma_ast::{
    ClassMember, ContentTagContent, ContentTagEnd, ContentTagExpression, ContentTagMember,
    ContentTagStart,
};
use swc_ecma_parser::lexer::ScannedContentTag;
use swc_ecma_visit::{Visit, VisitWith};

#[derive(Default, Debug)]
pub struct LocateContentTagVisitor {
    pub occurrences: Vec<Occurrence>,
    src: String,
    // Where the file starts in its source map. Spans are measured from here.
    start_pos: BytePos,
    is_ascii: bool,
}

#[derive(Eq, PartialEq, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
enum ContentTagKind {
    Expression,
    ClassMember,
}

impl LocateContentTagVisitor {
    pub fn new(src: String, start_pos: BytePos) -> Self {
        let is_ascii = src.is_ascii();
        Self {
            occurrences: Default::default(),
            src,
            start_pos,
            is_ascii,
        }
    }

    fn add_occurrence(
        &mut self,
        kind: ContentTagKind,
        span: &Span,
        opening: &ContentTagStart,
        contents: &ContentTagContent,
        closing: &ContentTagEnd,
    ) {
        let occurrence = Occurrence {
            kind,
            tag_name: self.tag_name(opening.span),
            contents: contents.value.to_string(),
            range: Range::new(&self.src, self.start_pos, span, self.is_ascii),
            start_range: Range::new(&self.src, self.start_pos, &opening.span, self.is_ascii),
            content_range: Range::new(&self.src, self.start_pos, &contents.span, self.is_ascii),
            end_range: Range::new(&self.src, self.start_pos, &closing.span, self.is_ascii),
        };

        self.occurrences.push(occurrence);
    }
}

impl LocateContentTagVisitor {
    // The syntax tree does not record a content tag's name, so it is read
    // from its opening tag, `<name>`.
    fn tag_name(&self, opening: Span) -> String {
        let start = (opening.lo - self.start_pos).0 as usize;
        let end = (opening.hi - self.start_pos).0 as usize;
        self.src[start + 1..end - 1].to_owned()
    }
}

impl Visit for LocateContentTagVisitor {
    fn visit_expr(&mut self, n: &swc_ecma_ast::Expr) {
        match n {
            swc_ecma_ast::Expr::ContentTagExpression(ContentTagExpression {
                span,
                opening,
                contents,
                closing,
            }) => {
                self.add_occurrence(ContentTagKind::Expression, span, opening, contents, closing);
            }
            _ => {}
        }

        n.visit_children_with(self);
    }

    fn visit_class_member(&mut self, n: &ClassMember) {
        match n {
            ClassMember::ContentTagMember(ContentTagMember {
                span,
                opening,
                contents,
                closing,
            }) => {
                self.add_occurrence(
                    ContentTagKind::ClassMember,
                    span,
                    opening,
                    contents,
                    closing,
                );
            }
            _ => {}
        }

        n.visit_children_with(self);
    }
}

/// Builds an occurrence for a content tag that swc's lexer found without
/// parsing, in a file the parser could not read. Whether it is a class member
/// is a guess.
pub fn scanned_occurrence(src: &str, tag: &ScannedContentTag, is_ascii: bool) -> Occurrence {
    let range = |start, end| Range::from_offsets(src, start, end, is_ascii);
    let open = &tag.open;
    Occurrence {
        kind: if tag.in_class_body {
            ContentTagKind::ClassMember
        } else {
            ContentTagKind::Expression
        },
        tag_name: open.name.clone(),
        contents: src[open.content_start..tag.end].to_owned(),
        range: range(open.start, tag.close_end),
        start_range: range(open.start, open.content_start),
        content_range: range(open.content_start, tag.end),
        end_range: range(tag.end, tag.close_end),
    }
}

impl Occurrence {
    /// The tag's name, such as `template`.
    pub fn tag_name(&self) -> &str {
        &self.tag_name
    }

    /// The text between the opening and closing tags.
    pub fn contents(&self) -> &str {
        &self.contents
    }

    /// Whether the tag is directly inside a class body.
    pub fn is_class_member(&self) -> bool {
        self.kind == ContentTagKind::ClassMember
    }

    /// The byte offsets where the whole tag starts and ends.
    pub fn byte_range(&self) -> (usize, usize) {
        (self.range.start_byte, self.range.end_byte)
    }
}

impl Range {
    pub fn start_byte(&self) -> usize {
        self.start_byte
    }

    pub fn end_byte(&self) -> usize {
        self.end_byte
    }
}

#[derive(Serialize, Debug, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    #[serde(rename = "type")]
    kind: ContentTagKind,
    tag_name: String,
    contents: String,
    range: Range,
    // the span of the opening "<template>" tag
    start_range: Range,
    content_range: Range,
    // the span of the closing "</template>" tag
    end_range: Range,
}

#[derive(Serialize, Debug, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Range {
    start_byte: usize,
    end_byte: usize,
    start_char: usize,
    end_char: usize,
    start_utf16_codepoint: usize,
    end_utf16_codepoint: usize,
}
impl Range {
    pub fn new(src: &str, start_pos: BytePos, span: &Span, is_ascii: bool) -> Range {
        Self::from_offsets(
            src,
            (span.lo - start_pos).0 as usize,
            (span.hi - start_pos).0 as usize,
            is_ascii,
        )
    }

    /// The range from `start_byte` to `end_byte` in `src`.
    pub fn from_offsets(src: &str, start_byte: usize, end_byte: usize, is_ascii: bool) -> Range {
        if is_ascii {
            // For ASCII sources, byte/char/utf16 offsets are all identical.
            Range {
                start_byte,
                end_byte,
                start_char: start_byte,
                end_char: end_byte,
                start_utf16_codepoint: start_byte,
                end_utf16_codepoint: end_byte,
            }
        } else {
            Range {
                start_byte,
                end_byte,
                start_char: src[..start_byte].chars().count(),
                end_char: src[..end_byte].chars().count(),
                start_utf16_codepoint: src[..start_byte].encode_utf16().count(),
                end_utf16_codepoint: src[..end_byte].encode_utf16().count(),
            }
        }
    }
}

#[cfg(test)]
use crate::Preprocessor;

#[test]
fn test_basic_example() {
    let p = Preprocessor::new();
    let output = p
        .parse("<template>Hello!</template>", Default::default())
        .unwrap();
    let expected = Occurrence {
        kind: ContentTagKind::Expression,
        tag_name: "template".into(),
        contents: "Hello!".into(),
        range: Range {
            start_byte: 0,
            end_byte: 27,
            start_char: 0,
            end_char: 27,
            start_utf16_codepoint: 0,
            end_utf16_codepoint: 27,
        },
        start_range: Range {
            start_byte: 0,
            end_byte: 10,
            start_char: 0,
            end_char: 10,
            start_utf16_codepoint: 0,
            end_utf16_codepoint: 10,
        },
        content_range: Range {
            start_byte: 10,
            end_byte: 16,
            start_char: 10,
            end_char: 16,
            start_utf16_codepoint: 10,
            end_utf16_codepoint: 16,
        },
        end_range: Range {
            start_byte: 16,
            end_byte: 27,
            start_char: 16,
            end_char: 27,
            start_utf16_codepoint: 16,
            end_utf16_codepoint: 27,
        },
    };
    assert_eq!(output, vec![expected]);
}

#[test]
fn test_expression_position() {
    let p = Preprocessor::new();
    let output = p
        .parse(
            "const tpl = <template>Hello!</template>",
            Default::default(),
        )
        .unwrap();

    let expected = vec![Occurrence {
        kind: ContentTagKind::Expression,
        tag_name: "template".into(),
        contents: "Hello!".into(),
        range: Range {
            start_byte: 12,
            end_byte: 39,
            start_char: 12,
            end_char: 39,
            start_utf16_codepoint: 12,
            end_utf16_codepoint: 39,
        },
        start_range: Range {
            start_byte: 12,
            end_byte: 22,
            start_char: 12,
            end_char: 22,
            start_utf16_codepoint: 12,
            end_utf16_codepoint: 22,
        },
        content_range: Range {
            start_byte: 22,
            end_byte: 28,
            start_char: 22,
            end_char: 28,
            start_utf16_codepoint: 22,
            end_utf16_codepoint: 28,
        },
        end_range: Range {
            start_byte: 28,
            end_byte: 39,
            start_char: 28,
            end_char: 39,
            start_utf16_codepoint: 28,
            end_utf16_codepoint: 39,
        },
    }];

    assert_eq!(output, expected);
}

#[test]
fn test_inside_class_body() {
    let p = Preprocessor::new();
    let output = p
        .parse(
            r#"
                  class A {
                    <template>Hello!</template>
                  }
                "#,
            Default::default(),
        )
        .unwrap();

    let expected = vec![Occurrence {
        kind: ContentTagKind::ClassMember,
        tag_name: "template".into(),
        contents: "Hello!".into(),
        range: Range {
            start_byte: 49,
            end_byte: 76,
            start_char: 49,
            end_char: 76,
            start_utf16_codepoint: 49,
            end_utf16_codepoint: 76,
        },
        start_range: Range {
            start_byte: 49,
            end_byte: 59,
            start_char: 49,
            end_char: 59,
            start_utf16_codepoint: 49,
            end_utf16_codepoint: 59,
        },
        content_range: Range {
            start_byte: 59,
            end_byte: 65,
            start_char: 59,
            end_char: 65,
            start_utf16_codepoint: 59,
            end_utf16_codepoint: 65,
        },
        end_range: Range {
            start_byte: 65,
            end_byte: 76,
            start_char: 65,
            end_char: 76,
            start_utf16_codepoint: 65,
            end_utf16_codepoint: 76,
        },
    }];

    assert_eq!(output, expected);
}

#[test]
fn test_multibyte_character_inside_template() {
    let p = Preprocessor::new();
    let output = p
        .parse(
            r#"
                  class A {
                    <template>Hell😀!</template>
                  }
                "#,
            Default::default(),
        )
        .unwrap();

    let expected = vec![Occurrence {
        kind: ContentTagKind::ClassMember,
        tag_name: "template".into(),
        contents: "Hell😀!".into(),
        range: Range {
            start_byte: 49,
            end_byte: 79,
            start_char: 49,
            end_char: 76,
            start_utf16_codepoint: 49,
            end_utf16_codepoint: 77,
        },
        start_range: Range {
            start_byte: 49,
            end_byte: 59,
            start_char: 49,
            end_char: 59,
            start_utf16_codepoint: 49,
            end_utf16_codepoint: 59,
        },
        content_range: Range {
            start_byte: 59,
            end_byte: 68,
            start_char: 59,
            end_char: 65,
            start_utf16_codepoint: 59,
            end_utf16_codepoint: 66,
        },
        end_range: Range {
            start_byte: 68,
            end_byte: 79,
            start_char: 65,
            end_char: 76,
            start_utf16_codepoint: 66,
            end_utf16_codepoint: 77,
        },
    }];

    assert_eq!(output, expected);
}

#[test]
fn test_preceded_by_a_slash_character() {
    let p = Preprocessor::new();
    // What is this testing?
    // Would a better test be:
    // `const divide = 1 / <template>Hello!</template>;`
    let output = p
        .parse(
            r#"
                  const divide = () => 4 / 2;
                  <template>Hello!</template>
                "#,
            Default::default(),
        )
        .unwrap();

    let expected = vec![Occurrence {
        kind: ContentTagKind::Expression,
        tag_name: "template".into(),
        contents: "Hello!".into(),
        range: Range {
            start_byte: 65,
            end_byte: 92,
            start_char: 65,
            end_char: 92,
            start_utf16_codepoint: 65,
            end_utf16_codepoint: 92,
        },
        start_range: Range {
            start_byte: 65,
            end_byte: 75,
            start_char: 65,
            end_char: 75,
            start_utf16_codepoint: 65,
            end_utf16_codepoint: 75,
        },
        content_range: Range {
            start_byte: 75,
            end_byte: 81,
            start_char: 75,
            end_char: 81,
            start_utf16_codepoint: 75,
            end_utf16_codepoint: 81,
        },
        end_range: Range {
            start_byte: 81,
            end_byte: 92,
            start_char: 81,
            end_char: 92,
            start_utf16_codepoint: 81,
            end_utf16_codepoint: 92,
        },
    }];

    assert_eq!(output, expected);
}

#[test]
fn test_template_inside_a_regexp() {
    let p = Preprocessor::new();
    let output = p
        .parse(
            r#"
                  const myregex = /<template>/;
                  <template>Hello!</template>
                "#,
            Default::default(),
        )
        .unwrap();

    let expected = vec![Occurrence {
        kind: ContentTagKind::Expression,
        tag_name: "template".into(),
        contents: "Hello!".into(),
        range: Range {
            start_byte: 67,
            end_byte: 94,
            start_char: 67,
            end_char: 94,
            start_utf16_codepoint: 67,
            end_utf16_codepoint: 94,
        },
        start_range: Range {
            start_byte: 67,
            end_byte: 77,
            start_char: 67,
            end_char: 77,
            start_utf16_codepoint: 67,
            end_utf16_codepoint: 77,
        },
        content_range: Range {
            start_byte: 77,
            end_byte: 83,
            start_char: 77,
            end_char: 83,
            start_utf16_codepoint: 77,
            end_utf16_codepoint: 83,
        },
        end_range: Range {
            start_byte: 83,
            end_byte: 94,
            start_char: 83,
            end_char: 94,
            start_utf16_codepoint: 83,
            end_utf16_codepoint: 94,
        },
    }];

    assert_eq!(output, expected);
}

#[test]
fn test_no_match() {
    let p = Preprocessor::new();
    let output = p
        .parse("console.log('Hello world');", Default::default())
        .unwrap();

    assert_eq!(output, vec![]);
}

#[test]
fn test_inner_expression() {
    let p = Preprocessor::new();
    let src = r#"let x = doIt(<template>Hello</template>)"#;
    let output = p.parse(src, Default::default()).unwrap();

    assert_eq!(
        output,
        vec![Occurrence {
            range: Range {
                start_byte: 13,
                end_byte: 39,
                start_char: 13,
                end_char: 39,
                start_utf16_codepoint: 13,
                end_utf16_codepoint: 39
            },
            content_range: Range {
                start_byte: 23,
                end_byte: 28,
                start_char: 23,
                end_char: 28,
                start_utf16_codepoint: 23,
                end_utf16_codepoint: 28
            },
            contents: "Hello".into(),
            end_range: Range {
                start_byte: 28,
                end_byte: 39,
                start_char: 28,
                end_char: 39,
                start_utf16_codepoint: 28,
                end_utf16_codepoint: 39
            },
            start_range: Range {
                start_byte: 13,
                end_byte: 23,
                start_char: 13,
                end_char: 23,
                start_utf16_codepoint: 13,
                end_utf16_codepoint: 23
            },
            tag_name: "template".into(),
            kind: ContentTagKind::Expression
        }]
    );
}

// For an ASCII source, every kind of offset in a range is the same.
#[cfg(test)]
fn ascii_range(start: usize, end: usize) -> Range {
    Range {
        start_byte: start,
        end_byte: end,
        start_char: start,
        end_char: end,
        start_utf16_codepoint: start,
        end_utf16_codepoint: end,
    }
}

// A preprocessor that reads `tags`, with no output.
#[cfg(test)]
fn reading(tags: &[&str]) -> Preprocessor {
    Preprocessor::with_config(crate::Config {
        parser: Some(Box::new(crate::TagNames::new(tags.iter().copied()))),
    })
    .unwrap()
}

#[test]
fn test_custom_tag_name() {
    let p = reading(&["jsx"]);
    let output = p.parse("<jsx>hi</jsx>", Default::default()).unwrap();
    assert_eq!(
        output,
        vec![Occurrence {
            kind: ContentTagKind::Expression,
            tag_name: "jsx".into(),
            contents: "hi".into(),
            range: ascii_range(0, 13),
            start_range: ascii_range(0, 5),
            content_range: ascii_range(5, 7),
            end_range: ascii_range(7, 13),
        }]
    );
}

#[test]
fn test_custom_tag_name_ignores_template() {
    let p = reading(&["jsx"]);
    let output = p.parse("let x = <template>hi</template>", Default::default());
    assert!(
        output.map_or(true, |occurrences| occurrences.is_empty()),
        "<template> should not be read as a content tag"
    );
}

#[test]
fn test_default_tag_name_ignores_other_tags() {
    let p = Preprocessor::new();
    let output = p.parse("let x = <jsx>hi</jsx>", Default::default());
    assert!(
        output.map_or(true, |occurrences| occurrences.is_empty()),
        "<jsx> should not be read as a content tag"
    );
}

#[test]
fn test_several_tag_names() {
    let p = reading(&["template", "style"]);
    let output = p
        .parse(
            "let a = <template>x</template>;\nlet b = <style>y</style>;",
            Default::default(),
        )
        .unwrap();
    assert_eq!(
        output
            .iter()
            .map(|o| (o.tag_name.as_str(), o.contents.as_str()))
            .collect::<Vec<_>>(),
        vec![("template", "x"), ("style", "y")]
    );
}

#[test]
fn test_default_parser_ends_at_the_first_closing_tag() {
    let p = Preprocessor::new();
    let output = p
        .parse(
            "class A { <template><template>a</template></template> }",
            Default::default(),
        )
        .map(|occurrences| occurrences[0].contents.clone());
    assert_eq!(output.ok(), None, "the second closing tag is a parse error");
}

// Ends each content tag at its last closing tag in the file.
#[cfg(test)]
struct LastClosingTag(Vec<String>);

#[cfg(test)]
impl crate::ContentTagParser for LastClosingTag {
    fn tags(&self) -> &[String] {
        &self.0
    }

    fn find_end(
        &self,
        source: &str,
        open: &crate::OpenTag,
    ) -> Result<usize, crate::ContentTagError> {
        source
            .rfind(&format!("</{}>", open.name))
            .ok_or_else(|| crate::ContentTagError::new(open.content_start, "no closing tag"))
    }
}

#[test]
fn test_parser_decides_where_the_content_ends() {
    let p = Preprocessor::with_config(crate::Config {
        parser: Some(Box::new(LastClosingTag(vec!["template".into()]))),
    })
    .unwrap();
    let src = r#"class A { <template><table><template id="row"><tr></tr></template></table></template> }"#;
    let output = p.parse(src, Default::default()).unwrap();
    assert_eq!(
        output,
        vec![Occurrence {
            kind: ContentTagKind::ClassMember,
            tag_name: "template".into(),
            contents: r#"<table><template id="row"><tr></tr></template></table>"#.into(),
            range: ascii_range(10, 85),
            start_range: ascii_range(10, 20),
            content_range: ascii_range(20, 74),
            end_range: ascii_range(74, 85),
        }]
    );
}

// A preprocessor can be reused. Each parse starts from a fresh source map, so
// earlier files do not shift the ranges of later ones.
#[test]
fn test_reused_preprocessor_gives_the_same_ranges() {
    let p = Preprocessor::new();
    let ascii = "let x = <template>Hello</template>";
    let unicode = "let ü = 1;\nlet x = <template>💩</template>";
    let longer = format!("{}<template>Hi</template>", "// padding\n".repeat(50));

    let first_ascii = p.parse(ascii, Default::default()).unwrap();
    let first_unicode = p.parse(unicode, Default::default()).unwrap();
    for _ in 0..3 {
        p.parse(&longer, Default::default()).unwrap();
        assert_eq!(p.parse(ascii, Default::default()).unwrap(), first_ascii);
        assert_eq!(p.parse(unicode, Default::default()).unwrap(), first_unicode);
    }
    assert_eq!(first_unicode[0].range.start_char, 19);
}
