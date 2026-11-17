use crate::{
    find_first_closing_tag, Config, ContentTagError, ContentTagParser, Imports, Occurrence,
    OpenTag, Options, Preprocessor as CorePreprocessor, SpliceError,
};
use js_sys::{Function, JsString, Object, Reflect};
use std::cell::RefCell;
use std::collections::HashMap;
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
    // The `transform` of each tag in the `tags` option. When there are any,
    // `process` replaces each tag with what its transform returns.
    transforms: HashMap<String, Function>,
}

// What the constructor options hold.
struct ConstructorOptions {
    config: Config,
    js_parser: Option<Rc<JsParser>>,
    transforms: HashMap<String, Function>,
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
fn constructor_options(options: &JsValue) -> Result<ConstructorOptions, JsValue> {
    if options.is_undefined() || options.is_null() {
        return Ok(ConstructorOptions {
            config: Config::default(),
            js_parser: None,
            transforms: HashMap::new(),
        });
    }
    if !options.is_object() {
        return Err(js_type_error(
            "Preprocessor options must be an object".into(),
        ));
    }

    let tags = Reflect::get(options, &"tags".into())?;
    if tags.is_undefined() {
        return Ok(ConstructorOptions {
            config: Config::default(),
            js_parser: None,
            transforms: HashMap::new(),
        });
    }
    tags_option(&tags)
}

/// Reads the `tags` option, which is `{ [name]: { findEnd?, transform? } }`.
fn tags_option(tags: &JsValue) -> Result<ConstructorOptions, JsValue> {
    if !tags.is_object() {
        return Err(js_type_error(
            "The Preprocessor `tags` option must be an object with an entry for each tag".into(),
        ));
    }
    let mut names = vec![];
    let mut find_end = HashMap::new();
    let mut transforms = HashMap::new();
    for name in Object::keys(tags.unchecked_ref::<Object>()).iter() {
        let name = js_string(&name);
        let tag = Reflect::get(tags, &name.as_str().into())?;
        if !tag.is_object() {
            return Err(js_type_error(
                format!("The Preprocessor `tags.{name}` option must be an object").into(),
            ));
        }
        if let Some(function) = optional_function(&tag, "findEnd", &format!("tags.{name}.findEnd"))?
        {
            find_end.insert(name.clone(), function);
        }
        if let Some(function) =
            optional_function(&tag, "transform", &format!("tags.{name}.transform"))?
        {
            transforms.insert(name.clone(), function);
        }
        names.push(name);
    }
    if let Some(name) = names.iter().find(|name| !transforms.contains_key(*name)) {
        if !transforms.is_empty() {
            return Err(js_type_error(
                format!(
                    "The Preprocessor `tags.{name}` option needs a `transform`, because another tag has one"
                )
                .into(),
            ));
        }
    }

    let js_parser = Rc::new(JsParser {
        tags: names,
        find_end,
        source: RefCell::new(None),
    });
    Ok(ConstructorOptions {
        config: Config {
            parser: Some(Box::new(SharedJsParser(js_parser.clone()))),
        },
        js_parser: Some(js_parser),
        transforms,
    })
}

fn optional_function(
    object: &JsValue,
    key: &str,
    option: &str,
) -> Result<Option<Function>, JsValue> {
    let value = Reflect::get(object, &key.into())?;
    if value.is_undefined() {
        return Ok(None);
    }
    value.dyn_into::<Function>().map(Some).map_err(|_| {
        js_type_error(format!("The Preprocessor `{option}` option must be a function").into())
    })
}

/// A content tag parser given from JavaScript.
struct JsParser {
    tags: Vec<String>,
    // The `findEnd` for each tag name that has one.
    find_end: HashMap<String, Function>,
    // The JavaScript string of the current `parse` or `process` call, so
    // `findEnd` is given that string instead of a copy of it.
    source: RefCell<Option<JsValue>>,
}

impl ContentTagParser for JsParser {
    fn tags(&self) -> &[String] {
        &self.tags
    }

    fn find_end(&self, source: &str, open: &OpenTag) -> Result<usize, ContentTagError> {
        let Some(find_end) = self.find_end.get(&open.name) else {
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
                let missing_closing_tag = Reflect::get(&err, &"code".into())
                    .ok()
                    .and_then(|code| code.as_string())
                    .is_some_and(|code| code == "missing-closing-tag");
                if missing_closing_tag {
                    return Err(ContentTagError::missing_closing_tag(message));
                }
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

impl Preprocessor {
    fn process_with_transforms(&self, src: JsString, options: JsValue) -> Result<JsValue, JsValue> {
        let recover =
            js_boolean(&options) && js_boolean(&Reflect::get(&options, &"recover".into())?);
        let options = Options::new(options);

        // The object each transform is given as its second argument. Its
        // `import` is a Rust closure, made when the first tag is transformed.
        let context = Object::new();
        let mut import: Option<Closure<dyn FnMut(JsValue, JsValue) -> Result<JsValue, JsValue>>> =
            None;
        let transform = |tag: &Occurrence, imports: &Imports| -> Result<String, JsValue> {
            if import.is_none() {
                let imports = imports.clone();
                let closure = Closure::new(move |name: JsValue, from: JsValue| {
                    let (Some(name), Some(from)) = (name.as_string(), from.as_string()) else {
                        return Err(js_type_error(
                            "`import` takes the name to import and the module to import it from"
                                .into(),
                        ));
                    };
                    imports
                        .import(&name, &from)
                        .map(JsValue::from)
                        .map_err(|message| js_error(message.into()))
                });
                Reflect::set(&context, &"import".into(), closure.as_ref())?;
                import = Some(closure);
            }
            let transform = &self.transforms[tag.tag_name()];
            let tag = serde_json::to_string(tag).map_err(|err| js_error(err.to_string().into()))?;
            let text = transform.call2(&JsValue::NULL, &json_parse(tag.into()), &context)?;
            text.as_string()
                .ok_or_else(|| js_type_error("A tag's `transform` must return a string".into()))
        };

        let result = self.with_source(&src, |core, src| {
            core.process_with(src, options, recover, transform)
        });
        let spliced = match result {
            Ok(spliced) => spliced,
            Err(SpliceError::Parse(err)) => {
                return Err(as_javascript_error(err.error, err.source_map))
            }
            Err(SpliceError::Transform(err)) => return Err(err),
        };
        let json = |value: Result<String, serde_json::Error>| match value {
            Ok(serialized) => Ok(json_parse(serialized.into())),
            Err(err) => Err(js_error(err.to_string().into())),
        };
        let result = Object::new();
        Reflect::set(&result, &"code".into(), &spliced.code.into())?;
        Reflect::set(&result, &"map".into(), &spliced.map.into())?;
        Reflect::set(
            &result,
            &"tags".into(),
            &json(serde_json::to_string(&spliced.tags))?,
        )?;
        Reflect::set(
            &result,
            &"errors".into(),
            &json(serde_json::to_string(&spliced.errors))?,
        )?;
        Ok(result.into())
    }

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
        let ConstructorOptions {
            config,
            js_parser,
            transforms,
        } = constructor_options(&options)?;
        let core = CorePreprocessor::with_config(config)
            .map_err(|err| js_error(err.to_string().into()))?;
        Ok(Self {
            core,
            js_parser,
            transforms,
        })
    }

    /// Returns a plain `{ code, map }` object. The strings are copied into
    /// JavaScript, so nothing is left in wasm memory waiting for the
    /// JavaScript object to be garbage collected.
    ///
    /// With a `transform` for its tags, it replaces each tag with what its
    /// transform returns, and also returns `tags` and `errors`.
    pub fn process(&self, src: JsString, options: JsValue) -> Result<JsValue, JsValue> {
        if !self.transforms.is_empty() {
            return self.process_with_transforms(src, options);
        }
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
