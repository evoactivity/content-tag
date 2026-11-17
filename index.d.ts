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
   * The content tags to read, by name. Each one can say where its content
   * ends with `findEnd`, and what replaces it with `transform`. Default is
   * `{ template: {} }`.
   *
   * A name must start with an ASCII letter and contain only ASCII letters,
   * digits and hyphens. The opening tag must be exactly `<name>`.
   */
  tags?: Record<string, TagOptions>;
}

interface TagOptions {
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

  /**
   * Returns the code that `process` writes in place of the tag. The rest of
   * the source is copied as it is, and the code is not parsed or printed
   * again. When any tag has a `transform`, every tag needs one.
   *
   * Without a `transform` for any tag, `process` writes the Ember output for
   * `template`, which compiles the content with `@ember/template-compiler`.
   * It throws when it reaches a tag with another name.
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

interface OpenTag {
  /** The tag's name, such as `"template"`. */
  name: string;
  /** The index of the `<` of the opening tag. */
  start: number;
  /** The index just after the opening tag, where the content starts. */
  contentStart: number;
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
