//! Replaces each content tag with text that the caller gives for it.
//!
//! This is what `process` does when the caller transforms the tags itself.
//! The rest of the source is copied as it is, so it does not have to parse.

use std::cell::RefCell;
use std::rc::Rc;

use serde::Serialize;
use swc_common::{sync::Lrc, BytePos, LineCol, SourceMap};

use crate::locate::{Occurrence, Range};
use crate::RENAME_SUFFIX;
use crate::{
    encode_inline_source_map, new_source_file, Options, ParseError, Preprocessor, RecoveryError,
    SourceMapConfig,
};

/// What [Preprocessor::process_with] made.
pub struct Spliced {
    pub code: String,
    pub map: String,
    /// The content tags that were replaced, in source order.
    pub tags: Vec<SplicedTag>,
    /// The errors in the source. Empty unless `recover` was set.
    pub errors: Vec<RecoveryError>,
}

/// A content tag that was replaced.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SplicedTag {
    #[serde(flatten)]
    pub tag: Occurrence,
    /// Where the text that replaced the tag is in `code`.
    pub output_range: Range,
}

/// Why [Preprocessor::process_with] failed.
pub enum SpliceError<E> {
    /// The source has a syntax error, and `recover` was not set.
    Parse(ParseError),
    /// The transform returned this error.
    Transform(E),
}

/// The imports that the transforms of one file asked for.
///
/// It is a handle that can be cloned, so a transform written in another
/// language can keep one.
#[derive(Clone, Default)]
pub struct Imports(Rc<RefCell<Vec<Import>>>);

struct Import {
    name: String,
    from: String,
    local: String,
}

impl Imports {
    /// Asks for `import { name } from "from"`, and returns the local name to
    /// use for it. The local name cannot clash with a name in the file. Asking
    /// for the same import again returns the same local name. A `name` of
    /// `default` is the default export.
    pub fn import(&self, name: &str, from: &str) -> Result<String, String> {
        if !is_identifier(name) {
            return Err(format!("`{name}` is not a name that can be imported"));
        }
        let mut imports = self.0.borrow_mut();
        if let Some(import) = imports
            .iter()
            .find(|import| import.name == name && import.from == from)
        {
            return Ok(import.local.clone());
        }
        // The same name from another module needs a local name of its own.
        let same_name = imports.iter().filter(|import| import.name == name).count();
        let local = if same_name == 0 {
            format!("{name}_{RENAME_SUFFIX}")
        } else {
            format!("{name}_{RENAME_SUFFIX}_{same_name}")
        };
        imports.push(Import {
            name: name.to_owned(),
            from: from.to_owned(),
            local: local.clone(),
        });
        Ok(local)
    }

    // The import declarations, one on each line.
    fn declarations(&self) -> String {
        let mut code = String::new();
        for import in self.0.borrow().iter() {
            let from = serde_json::to_string(&import.from).expect("a string can be serialized");
            code.push_str(&format!(
                "import {{ {} as {} }} from {from};\n",
                import.name, import.local
            ));
        }
        code
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

// The output as it is built, with the position of its end and the source map
// entries so far.
#[derive(Default)]
struct Output {
    code: String,
    line: u32,
    // In UTF-16 code units, as source maps count columns.
    col: u32,
    mappings: Vec<(usize, LineCol)>,
}

impl Output {
    // Maps the end of the output to `offset` in the source.
    fn map(&mut self, offset: usize) {
        self.mappings.push((
            offset,
            LineCol {
                line: self.line,
                col: self.col,
            },
        ));
    }

    fn push(&mut self, c: char) {
        self.code.push(c);
        if c == '\n' {
            self.line += 1;
            self.col = 0;
        } else {
            self.col += c.len_utf16() as u32;
        }
    }

    // Copies `src[start..end]`, mapping the start of each run of characters
    // that are not whitespace to where it came from.
    fn copy(&mut self, src: &str, start: usize, end: usize) {
        let mut in_word = false;
        for (i, c) in src[start..end].char_indices() {
            if c.is_whitespace() {
                in_word = false;
            } else if !in_word {
                in_word = true;
                self.map(start + i);
            }
            self.push(c);
        }
    }

    // Writes `text` in place of the source at `offset`.
    fn replace(&mut self, offset: usize, text: &str) {
        self.map(offset);
        for c in text.chars() {
            self.push(c);
        }
    }
}

impl Preprocessor {
    /// Replaces each content tag in `src` with the text that `transform`
    /// returns for it, and copies the rest of `src` as it is.
    ///
    /// `transform` can ask for imports, which are added at the top of the
    /// output when the file has at least one content tag.
    ///
    /// With `recover`, a syntax error in `src` is returned in `errors` and the
    /// content tags are found without parsing, as
    /// [Preprocessor::parse_with_recovery] does. Without it, a syntax error
    /// fails.
    pub fn process_with<E>(
        &self,
        src: &str,
        options: Options,
        recover: bool,
        mut transform: impl FnMut(&Occurrence, &Imports) -> Result<String, E>,
    ) -> Result<Spliced, SpliceError<E>> {
        let (mut tags, errors) = if recover {
            let recovered = self.parse_with_recovery(
                src,
                Options {
                    filename: options.filename.clone(),
                    inline_source_map: false,
                },
            );
            (recovered.templates, recovered.errors)
        } else {
            let tags = self
                .parse(
                    src,
                    Options {
                        filename: options.filename.clone(),
                        inline_source_map: false,
                    },
                )
                .map_err(SpliceError::Parse)?;
            (tags, vec![])
        };
        // A class's own content tag is found before one in its heritage
        // clause, which comes earlier in the source.
        tags.sort_by_key(|tag| tag.byte_range().0);

        let imports = Imports::default();
        let mut output = Output::default();
        // For each tag, where its replacement is in the output.
        let mut replaced = vec![];
        let mut copied = 0;
        for tag in &tags {
            let (start, end) = tag.byte_range();
            output.copy(src, copied, start);
            let text = transform(tag, &imports).map_err(SpliceError::Transform)?;
            let output_start = output.code.len();
            output.replace(start, &text);
            replaced.push((output_start, output.code.len()));
            copied = end;
        }
        output.copy(src, copied, src.len());

        let declarations = imports.declarations();
        let import_lines = declarations.matches('\n').count() as u32;
        let code = declarations.clone() + &output.code;

        let source_map: Lrc<SourceMap> = Default::default();
        let source_file = new_source_file(&source_map, src, options.filename);
        let mappings: Vec<(BytePos, LineCol)> = output
            .mappings
            .iter()
            .map(|(offset, generated)| {
                (
                    source_file.start_pos + BytePos(*offset as u32),
                    LineCol {
                        line: generated.line + import_lines,
                        col: generated.col,
                    },
                )
            })
            .collect();
        let mut map = vec![];
        source_map
            .build_source_map(&mappings, None, SourceMapConfig {})
            .to_writer(&mut map)
            .expect("a source map can be written to memory");

        let is_ascii = code.is_ascii();
        let tags = tags
            .into_iter()
            .zip(replaced)
            .map(|(tag, (start, end))| SplicedTag {
                tag,
                output_range: Range::from_offsets(
                    &code,
                    declarations.len() + start,
                    declarations.len() + end,
                    is_ascii,
                ),
            })
            .collect();

        let code = if options.inline_source_map {
            code + &encode_inline_source_map(&map)
        } else {
            code
        };
        Ok(Spliced {
            code,
            map: String::from_utf8(map).expect("a source map is UTF-8"),
            tags,
            errors,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RecoveryErrorKind;

    // Replaces each tag with `<name:contents>`, or `[name:contents]` in a
    // class body.
    fn mark(tag: &Occurrence, _: &Imports) -> Result<String, String> {
        Ok(if tag.is_class_member() {
            format!("[{}:{}]", tag.tag_name(), tag.contents())
        } else {
            format!("<{}:{}>", tag.tag_name(), tag.contents())
        })
    }

    fn splice(src: &str, recover: bool) -> Spliced {
        match Preprocessor::new().process_with(src, Default::default(), recover, mark) {
            Ok(spliced) => spliced,
            Err(_) => panic!("expected {src:?} to be processed"),
        }
    }

    #[test]
    fn replaces_each_tag_and_copies_the_rest() {
        let src = "class A {\n  <template>a</template>\n}\nlet  x = <template>b</template> ;";
        let spliced = splice(src, false);
        assert_eq!(
            spliced.code,
            "class A {\n  [template:a]\n}\nlet  x = <template:b> ;"
        );
        assert!(spliced.errors.is_empty());
    }

    #[test]
    fn says_where_each_replacement_is_in_the_output() {
        let spliced = splice(
            "let ü = <template>a</template>;\nlet y = <template>b</template>;",
            false,
        );
        let replacements: Vec<&str> = spliced
            .tags
            .iter()
            .map(|tag| &spliced.code[tag.output_range.start_byte()..tag.output_range.end_byte()])
            .collect();
        assert_eq!(replacements, ["<template:a>", "<template:b>"]);
    }

    #[test]
    fn replaces_tags_in_source_order() {
        // The class's own tag is found before the one in its heritage clause.
        let src = "class A extends f(<template>a</template>) { <template>b</template> }";
        let spliced = splice(src, false);
        assert_eq!(
            spliced.code,
            "class A extends f(<template:a>) { [template:b] }"
        );
    }

    #[test]
    fn adds_each_import_once_at_the_top() {
        let src = "let x = <template>a</template>;\nlet y = <template>b</template>;";
        let spliced = Preprocessor::new()
            .process_with(src, Default::default(), false, |tag, imports| {
                let template = imports.import("template", "@ember/template-compiler")?;
                Ok::<_, String>(format!("{template}(`{}`)", tag.contents()))
            })
            .unwrap_or_else(|_| panic!("expected the source to be processed"));
        let local = format!("template_{RENAME_SUFFIX}");
        assert_eq!(
            spliced.code,
            format!(
                "import {{ template as {local} }} from \"@ember/template-compiler\";\nlet x = {local}(`a`);\nlet y = {local}(`b`);"
            )
        );
        // The ranges are in the output with its imports.
        let range = &spliced.tags[0].output_range;
        assert_eq!(
            &spliced.code[range.start_byte()..range.end_byte()],
            format!("{local}(`a`)")
        );
    }

    #[test]
    fn the_same_name_from_two_modules_gets_two_local_names() {
        let imports = Imports::default();
        let a = imports.import("x", "a").unwrap();
        let b = imports.import("x", "b").unwrap();
        assert_ne!(a, b);
        assert_eq!(imports.import("x", "a").unwrap(), a);
        assert_eq!(
            imports.declarations(),
            format!("import {{ x as {a} }} from \"a\";\nimport {{ x as {b} }} from \"b\";\n")
        );
    }

    #[test]
    fn imports_the_default_export_by_its_name() {
        let imports = Imports::default();
        let local = imports.import("default", "a").unwrap();
        assert_eq!(
            imports.declarations(),
            format!("import {{ default as {local} }} from \"a\";\n")
        );
    }

    #[test]
    fn rejects_a_name_that_cannot_be_imported() {
        assert!(Imports::default().import("a b", "x").is_err());
        assert!(Imports::default().import("", "x").is_err());
    }

    #[test]
    fn a_file_with_no_tags_gets_no_imports() {
        let spliced = splice("let x = 1;", false);
        assert_eq!(spliced.code, "let x = 1;");
        assert!(spliced.tags.is_empty());
    }

    #[test]
    fn a_syntax_error_fails_without_recover() {
        let result = Preprocessor::new().process_with("let x = ;", Default::default(), false, mark);
        assert!(matches!(result, Err(SpliceError::Parse(_))));
    }

    #[test]
    fn a_syntax_error_is_returned_with_recover() {
        let src = "let x = <template>a</template>;\nconst a =;\nlet y = <template>b</template>;";
        let spliced = splice(src, true);
        assert_eq!(
            spliced.code,
            "let x = <template:a>;\nconst a =;\nlet y = <template:b>;"
        );
        assert_eq!(spliced.errors.len(), 1);
        assert_eq!(spliced.errors[0].kind, RecoveryErrorKind::Syntax);
    }

    #[test]
    fn an_error_from_the_transform_is_returned() {
        let result = Preprocessor::new().process_with(
            "let x = <template>a</template>;",
            Default::default(),
            false,
            |_, _| Err::<String, _>("no"),
        );
        assert!(matches!(result, Err(SpliceError::Transform("no"))));
    }
}
