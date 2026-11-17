/*
 * wasm-pack doesn't give us correct enough types.
 * this file must be manually kept in sync with ./index.d.cts
 */

interface Range {
  startByte: number;
  endByte: number;
  startChar: number;
  endChar: number;
  startUtf16Codepoint: number;
  endUtf16Codepoint: number;
}

interface Parsed {
  type: "expression" | "class-member";
  /** The content tag's name, such as `"template"`. */
  tagName: string;
  contents: string;
  range: Range;
  contentRange: Range;
  startRange: Range;
  endRange: Range;
}

interface Recovered {
  templates: Parsed[];
  /**
   * Empty when the file parsed. Otherwise the parse error comes first,
   * followed by any errors from finding the content tags without parsing.
   */
  errors: RecoveryError[];
}

interface RecoveryError {
  /**
   * - `"syntax"`: the code outside the content tags does not parse.
   * - `"missing-closing-tag"`: a content tag has no closing tag. Its end is
   *   then a guess, or the tag is not in `templates` at all.
   * - `"tag-content"`: `findEnd` could not read the content of a tag, such as
   *   a comment in it that is not closed. The tag still ends at a closing tag.
   */
  kind: "syntax" | "missing-closing-tag" | "tag-content";
  message: string;
  /** For `"missing-closing-tag"`, the opening tag. */
  range: Range;
}

interface PreprocessorConstructorOptions {
  /**
   * The content tags to read, by name, such as `{ template: {} }`. Each one
   * can say where its content ends with `findEnd`, and what replaces it with
   * `transform`. It cannot be used with `parser` or `output`.
   *
   * A name must start with an ASCII letter and contain only ASCII letters,
   * digits and hyphens. The opening tag must be exactly `<name>`.
   */
  tags?: Record<string, TagOptions>;

  /**
   * Which content tags to read, and where each one's content ends. Default
   * is `{ tags: ["template"] }`.
   */
  parser?: ContentTagParser;

  /**
   * The code that `process` writes in place of each content tag, for each tag
   * name. `parse` does not need it. Default is the Ember output for
   * `template`, which compiles the content with `@ember/template-compiler`.
   * `process` throws when it reaches a tag with no output.
   */
  output?: Record<string, PreprocessorOutput>;
}

interface TagOptions {
  /**
   * Returns the index in `source` of the `<` of the tag that closes
   * `openTag`. See `ContentTagParser.findEnd`. Default is the first closing
   * tag after the content starts.
   */
  findEnd?(source: string, openTag: OpenTag): number;

  /**
   * Returns the code that `process` writes in place of the tag. The rest of
   * the source is copied as it is, and the code is not parsed or printed
   * again. When any tag has a `transform`, every tag needs one.
   *
   * Without a `transform` for any tag, `process` writes the Ember output for
   * `template`.
   */
  transform?(tag: Parsed, context: TransformContext): string;
}

interface TransformContext {
  /**
   * Adds `import { name } from "from"` to the top of the file, once, and
   * returns the local name to use for it. The local name cannot clash with a
   * name in the file. A `name` of `"default"` is the default export.
   */
  import(name: string, from: string): string;
}

/** A content tag that a `transform` replaced. */
interface Replaced extends Parsed {
  /** Where the code that replaced the tag is in `code`. */
  outputRange: Range;
}

interface ContentTagParser {
  /**
   * The names of the content tags to read, such as `["template"]`. The opening
   * tag must be exactly `<name>`. A name must start with an ASCII letter and
   * contain only ASCII letters, digits and hyphens.
   */
  tags: string[];

  /**
   * Returns the index in `source` of the `<` of the tag that closes
   * `openTag`. Indexes are UTF-16 code units, as in any JavaScript string.
   * Default is the first closing tag after the content starts.
   *
   * To report an error, throw an `Error`. If it has a numeric `offset`, the
   * error is reported at that index. If its `code` is
   * `"missing-closing-tag"`, the error is reported on the opening tag.
   */
  findEnd?(source: string, openTag: OpenTag): number;
}

interface OpenTag {
  /** The tag's name, such as `"template"`. */
  name: string;
  /** The index of the `<` of the opening tag. */
  start: number;
  /** The index just after the opening tag, where the content starts. */
  contentStart: number;
}

interface PreprocessorOutput {
  /**
   * Import declarations added to the top of a file that has at least one
   * content tag. Can be an empty string. The names they bind are renamed so
   * they cannot clash with names in the file, and uses of those names in
   * `expression` and `classMember` are renamed to match.
   */
  import: string;

  /**
   * The expression that replaces a content tag in an expression position.
   * Each `___CT_CONTENT___` in it is replaced with the content of the tag.
   * How it is written decides what replaces it:
   *
   * - `` `___CT_CONTENT___` `` becomes the content as a template literal.
   * - `"___CT_CONTENT___"` or `'___CT_CONTENT___'` becomes the content as a
   *   string literal.
   * - `___CT_CONTENT___` becomes the content, read as JavaScript code.
   *
   * It can be used any number of times, including none.
   */
  expression: string;

  /**
   * The class member that replaces a content tag in a class body. It uses
   * `___CT_CONTENT___` the same way as `expression`. An empty string removes
   * the tag. Default is `static { <expression>; }`.
   */
  classMember?: string;

  /**
   * Remove the indentation that all lines of the content share. Default is
   * `true`.
   */
  stripIndent?: boolean;

  /**
   * Turn a content tag that is a whole top-level statement into
   * `export default <expression>`. Default is `true`.
   */
  exportDefault?: boolean;
}

interface PreprocessorOptions {
  /** Default is `false` */
  inline_source_map?: boolean;

  filename?: string;

  /**
   * Only for `process` with a `transform`. Do not throw for a syntax error.
   * The error is returned in `errors`, and the content tags are found without
   * parsing the file, as `parseWithRecovery` does. Default is `false`.
   */
  recover?: boolean;
}

/**
 */
export class Preprocessor {
  free(): void;
  /**
   * @param {PreprocessorConstructorOptions | undefined} options
   */
  constructor(options?: PreprocessorConstructorOptions);
  /**
   * @param {string} src
   * @param {PreprocessorOptions | undefined} options
   * @returns {string}
   */
  process(
    src: string,
    options?: PreprocessorOptions,
  ): {
    code: string;
    map: string;
    /** With a `transform`, the content tags that were replaced. */
    tags?: Replaced[];
    /** With a `transform`, the errors in the source. Empty without `recover`. */
    errors?: RecoveryError[];
  };
  /**
   * @param {string} src
   * @param {PreprocessorOptions | undefined} options
   * @returns {any}
   */
  parse(src: string, options?: PreprocessorOptions): Parsed[];
  /**
   * Finds the content tags in `src` even when it has a syntax error, and does
   * not throw for one. When the file parses, `templates` is the same as
   * `parse` gives and `errors` is empty. When it does not, the templates are
   * found without parsing: their ranges and contents are the same, but
   * `type` is a guess.
   */
  parseWithRecovery(src: string, options?: PreprocessorOptions): Recovered;
}
