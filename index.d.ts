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

interface PreprocessorConstructorOptions {
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
   * error is reported at that index.
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
  ): { code: string; map: string };
  /**
   * @param {string} src
   * @param {PreprocessorOptions | undefined} options
   * @returns {any}
   */
  parse(src: string, options?: PreprocessorOptions): Parsed[];
}
