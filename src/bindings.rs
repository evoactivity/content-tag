use crate::{
    find_first_closing_tag, Config, ContentTagError, ContentTagParser, OpenTag, Options, Output,
    Preprocessor as CorePreprocessor,
};
use js_sys::{Array, Function, JsString, Object, Reflect};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use swc_common::{errors::Handler, sync::Lrc, SourceMap, Spanned};
use swc_error_reporters::{
    handler::{HandlerOpts, ThreadSafetyDiagnostics},
    ErrorEmitter, GraphicalReportHandler, GraphicalTheme, ToPrettyDiagnostic,
};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = Error)]
    fn js_error(message: JsValue) -> JsValue;

    #[wasm_bindgen(js_namespace = JSON, js_name = parse)]
    fn json_parse(value: JsValue) -> JsValue;

    #[wasm_bindgen(js_name = Boolean)]
    fn js_boolean(value: &JsValue) -> bool;

    #[wasm_bindgen(js_name = String)]
    fn js_string(value: &JsValue) -> String;

    #[wasm_bindgen(js_name = TypeError)]
    fn js_type_error(message: JsValue) -> JsValue;
}

impl Options {
    pub fn new(options: JsValue) -> Self {
        if js_boolean(&options) {
            // unwrapping here beacuse we already checked truthiness of
            // `options`, so the normal case of not passing any options has been
            // handled and this will only fail in unusual cases (like a
            // Javascript getter throwing)
            let option_filename = Reflect::get(&options, &"filename".into()).unwrap();
            let filename = if js_boolean(&option_filename) {
                Some(PathBuf::from(js_string(&option_filename)))
            } else {
                None
            };

            Self {
                // unwrap is justified here for the same reasons as commented above
                inline_source_map: js_boolean(
                    &Reflect::get(&options, &"inline_source_map".into()).unwrap(),
                ),
                filename,
            }
        } else {
            Self {
                inline_source_map: false,
                filename: None,
            }
        }
    }
}

#[wasm_bindgen]
pub struct Preprocessor {
    core: CorePreprocessor,
    // The parser given in the options, if any, which needs to see the source
    // of each call.
    js_parser: Option<Rc<JsParser>>,
}

fn capture_err_detail(
    err: swc_ecma_parser::error::Error,
    source_map: Lrc<SourceMap>,
    theme: GraphicalTheme,
) -> JsValue {
    let mut diagnostics = ThreadSafetyDiagnostics::default();
    let emitter = ErrorEmitter {
        diagnostics: diagnostics.clone(),
        cm: source_map.clone(),
        opts: HandlerOpts::default(),
    };
    let handler = Handler::with_emitter(true, false, Box::new(emitter));
    err.into_diagnostic(&handler).emit();
    let reporter = GraphicalReportHandler::new_themed(theme);
    let s: String = diagnostics
        .take()
        .iter()
        .map(|d| d.to_pretty_string(&source_map, false, &reporter))
        .collect();
    s.into()
}

fn as_javascript_error(err: swc_ecma_parser::error::Error, source_map: Lrc<SourceMap>) -> JsValue {
    let short_desc = format!("Parse Error at {}", source_map.span_to_string(err.span()));
    let js_err = js_error(short_desc.into());
    js_sys::Reflect::set(
        &js_err,
        &"source_code".into(),
        &capture_err_detail(
            err.clone(),
            source_map.clone(),
            GraphicalTheme::unicode_nocolor(),
        ),
    )
    .unwrap();
    js_sys::Reflect::set(
        &js_err,
        &"source_code_color".into(),
        &capture_err_detail(err, source_map, GraphicalTheme::unicode()),
    )
    .unwrap();
    return js_err;
}

/// Reads the options given to the `Preprocessor` constructor.
fn constructor_options(options: &JsValue) -> Result<(Config, Option<Rc<JsParser>>), JsValue> {
    if options.is_undefined() || options.is_null() {
        return Ok((Config::default(), None));
    }
    if !options.is_object() {
        return Err(js_type_error(
            "Preprocessor options must be an object".into(),
        ));
    }

    let js_parser = match Reflect::get(options, &"parser".into())? {
        parser if parser.is_undefined() => None,
        parser => Some(Rc::new(JsParser::new(&parser)?)),
    };

    let output = Reflect::get(options, &"output".into())?;
    let output = if output.is_undefined() {
        None
    } else {
        if !output.is_object() {
            return Err(js_type_error(
                "The Preprocessor `output` option must be an object with an output for each tag"
                    .into(),
            ));
        }
        let mut outputs = vec![];
        for tag in Object::keys(output.unchecked_ref::<Object>()).iter() {
            let tag = js_string(&tag);
            let value = Reflect::get(&output, &tag.as_str().into())?;
            outputs.push((tag.clone(), read_output(&value, &format!("output.{tag}"))?));
        }
        Some(outputs)
    };

    let config = Config {
        parser: js_parser
            .clone()
            .map(|parser| Box::new(SharedJsParser(parser)) as Box<dyn ContentTagParser>),
        output,
    };
    Ok((config, js_parser))
}

fn read_output(output: &JsValue, option: &str) -> Result<Output, JsValue> {
    if !output.is_object() {
        return Err(js_type_error(
            format!("The Preprocessor `{option}` option must be an object").into(),
        ));
    }
    Ok(Output {
        import: required_string(output, "import", &format!("{option}.import"))?,
        expression: required_string(output, "expression", &format!("{option}.expression"))?,
        class_member: optional_string(output, "classMember", &format!("{option}.classMember"))?,
        strip_indent: optional_bool(output, "stripIndent", &format!("{option}.stripIndent"))?
            .unwrap_or(true),
        export_default: optional_bool(output, "exportDefault", &format!("{option}.exportDefault"))?
            .unwrap_or(true),
    })
}

/// A content tag parser given from JavaScript as `{ tags, findEnd? }`.
struct JsParser {
    tags: Vec<String>,
    find_end: Option<Function>,
    // The JavaScript string of the current `parse` or `process` call, so
    // `findEnd` is given that string instead of a copy of it.
    source: RefCell<Option<JsValue>>,
}

impl JsParser {
    fn new(parser: &JsValue) -> Result<Self, JsValue> {
        if !parser.is_object() {
            return Err(js_type_error(
                "The Preprocessor `parser` option must be an object".into(),
            ));
        }
        let tags = Reflect::get(parser, &"tags".into())?;
        if !Array::is_array(&tags) {
            return Err(js_type_error(
                "The Preprocessor `parser.tags` option must be an array of strings".into(),
            ));
        }
        let tags = Array::from(&tags)
            .iter()
            .map(|tag| {
                tag.as_string().ok_or_else(|| {
                    js_type_error(
                        "The Preprocessor `parser.tags` option must be an array of strings".into(),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let find_end = match Reflect::get(parser, &"findEnd".into())? {
            find_end if find_end.is_undefined() => None,
            find_end => Some(find_end.dyn_into::<Function>().map_err(|_| {
                js_type_error("The Preprocessor `parser.findEnd` option must be a function".into())
            })?),
        };
        Ok(Self {
            tags,
            find_end,
            source: RefCell::new(None),
        })
    }
}

impl ContentTagParser for JsParser {
    fn tags(&self) -> &[String] {
        &self.tags
    }

    fn find_end(&self, source: &str, open: &OpenTag) -> Result<usize, ContentTagError> {
        let Some(find_end) = &self.find_end else {
            return find_first_closing_tag(source, open);
        };
        let js_source = self
            .source
            .borrow()
            .clone()
            .unwrap_or_else(|| JsValue::from_str(source));
        let open_tag = Object::new();
        let set = |key: &str, value: JsValue| Reflect::set(&open_tag, &key.into(), &value);
        let _ = set("name", open.name.as_str().into());
        let _ = set("start", (byte_to_utf16(source, open.start) as f64).into());
        let _ = set(
            "contentStart",
            (byte_to_utf16(source, open.content_start) as f64).into(),
        );

        match find_end.call2(&JsValue::NULL, &js_source, &open_tag) {
            Ok(end) => end
                .as_f64()
                .filter(|end| *end >= 0.0 && end.fract() == 0.0)
                .and_then(|end| utf16_to_byte(source, end as usize))
                .ok_or_else(|| {
                    ContentTagError::new(
                        open.content_start,
                        "`findEnd` must return the index of the closing tag",
                    )
                }),
            Err(err) => {
                let message = Reflect::get(&err, &"message".into())
                    .ok()
                    .and_then(|message| message.as_string())
                    .unwrap_or_else(|| js_string(&err));
                let offset = Reflect::get(&err, &"offset".into())
                    .ok()
                    .and_then(|offset| offset.as_f64())
                    .filter(|offset| *offset >= 0.0)
                    .and_then(|offset| utf16_to_byte(source, offset as usize))
                    .unwrap_or(open.content_start);
                Err(ContentTagError::new(offset, message))
            }
        }
    }
}

/// Shares a [JsParser] between the core preprocessor and the binding, which
/// sets the source of each call.
struct SharedJsParser(Rc<JsParser>);

impl ContentTagParser for SharedJsParser {
    fn tags(&self) -> &[String] {
        self.0.tags()
    }

    fn find_end(&self, source: &str, open: &OpenTag) -> Result<usize, ContentTagError> {
        self.0.find_end(source, open)
    }
}

// JavaScript indexes strings by UTF-16 code unit. These convert between those
// indexes and byte offsets.
fn byte_to_utf16(source: &str, byte: usize) -> usize {
    if source.is_ascii() {
        byte
    } else {
        source[..byte].encode_utf16().count()
    }
}

// Returns `None` for an index past the end, or in the middle of a character.
fn utf16_to_byte(source: &str, index: usize) -> Option<usize> {
    if source.is_ascii() {
        return (index <= source.len()).then_some(index);
    }
    let mut utf16 = 0;
    for (byte, c) in source.char_indices() {
        if utf16 == index {
            return Some(byte);
        }
        if utf16 > index {
            return None;
        }
        utf16 += c.len_utf16();
    }
    (utf16 == index).then_some(source.len())
}

fn optional_string(object: &JsValue, key: &str, option: &str) -> Result<Option<String>, JsValue> {
    let value = Reflect::get(object, &key.into())?;
    if value.is_undefined() {
        return Ok(None);
    }
    match value.as_string() {
        Some(value) => Ok(Some(value)),
        None => Err(js_type_error(
            format!("The Preprocessor `{option}` option must be a string").into(),
        )),
    }
}

fn required_string(object: &JsValue, key: &str, option: &str) -> Result<String, JsValue> {
    optional_string(object, key, option)?.ok_or_else(|| {
        js_type_error(
            format!("The Preprocessor `{option}` option is required when `output` is given").into(),
        )
    })
}

fn optional_bool(object: &JsValue, key: &str, option: &str) -> Result<Option<bool>, JsValue> {
    let value = Reflect::get(object, &key.into())?;
    if value.is_undefined() {
        return Ok(None);
    }
    match value.as_bool() {
        Some(value) => Ok(Some(value)),
        None => Err(js_type_error(
            format!("The Preprocessor `{option}` option must be a boolean").into(),
        )),
    }
}

impl Preprocessor {
    // Runs `f` with the source as a Rust string, while the JavaScript parser,
    // if there is one, can see the original JavaScript string.
    fn with_source<T>(&self, src: &JsString, f: impl FnOnce(&CorePreprocessor, &str) -> T) -> T {
        let rust_src = String::from(src);
        if let Some(parser) = &self.js_parser {
            *parser.source.borrow_mut() = Some(src.into());
        }
        let result = f(&self.core, &rust_src);
        if let Some(parser) = &self.js_parser {
            *parser.source.borrow_mut() = None;
        }
        result
    }
}

#[wasm_bindgen]
impl Preprocessor {
    #[wasm_bindgen(constructor)]
    pub fn new(options: JsValue) -> Result<Preprocessor, JsValue> {
        let (config, js_parser) = constructor_options(&options)?;
        let core = CorePreprocessor::with_config(config)
            .map_err(|err| js_error(err.to_string().into()))?;
        Ok(Self { core, js_parser })
    }

    /// Returns a plain `{ code, map }` object. The strings are copied into
    /// JavaScript, so nothing is left in wasm memory waiting for the
    /// JavaScript object to be garbage collected.
    pub fn process(&self, src: JsString, options: JsValue) -> Result<JsValue, JsValue> {
        let options = Options::new(options);
        let result = self.with_source(&src, |core, src| core.process(src, options));

        match result {
            Ok(output) => {
                let result = js_sys::Object::new();
                Reflect::set(&result, &"code".into(), &output.code.into())?;
                Reflect::set(&result, &"map".into(), &output.map.into())?;
                Ok(result.into())
            }
            Err(err) => Err(as_javascript_error(err.error, err.source_map)),
        }
    }

    /// Returns `{ templates, errors }`, and does not throw for a syntax
    /// error. When the file has one, the templates are found without
    /// parsing it.
    #[wasm_bindgen(js_name = parseWithRecovery)]
    pub fn parse_with_recovery(&self, src: JsString, options: JsValue) -> Result<JsValue, JsValue> {
        let options = Options::new(options);
        let recovered = self.with_source(&src, |core, src| core.parse_with_recovery(src, options));
        match serde_json::to_string(&recovered) {
            Ok(serialized) => Ok(json_parse(serialized.into())),
            Err(err) => Err(js_error(
                format!("Unexpected serialization error; please open an issue with the following debug info: {err:#?}").into(),
            )),
        }
    }

    pub fn parse(&self, src: JsString, options: JsValue) -> Result<JsValue, JsValue> {
        let options = Options::new(options);
        let result = self.with_source(&src, |core, src| core.parse(src, options));

        match result {
            Ok(parsed) => {
                match serde_json::to_string(&parsed) {
                    Ok(serialized) => Ok(json_parse(serialized.into())),
                    Err(err) =>  Err(js_error(format!("Unexpected serialization error; please open an issue with the following debug info: {err:#?}").into()))
                }
            },
            Err(err) => Err(as_javascript_error(err.error, err.source_map))
        }
    }
}
