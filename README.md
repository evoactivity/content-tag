# content-tag

`content-tag` is a preprocessor for JS files that are using the content-tag proposal. This originated with Ember.js' GJS and GTS functionality. You can read more by [checking out the original RFC](https://rfcs.emberjs.com/id/0931-template-compiler-api/)

This preprocessor can be used to transform files using the `content-tag` spec to standard JS. It is built on top of [swc](https://swc.rs/) using Rust and is deployed as a wasm package.

## Installation

```sh
npm install content-tag
```

## Usage

### Node (CommonJS)

```js
let { Preprocessor } = require("content-tag");
let p = new Preprocessor();
let output = p.process("<template>Hi</template>");

console.log(output);
```

### Node (ESM)

```js
import { Preprocessor } from "content-tag";
let p = new Preprocessor();
let output = p.process("<template>Hi</template>");

console.log(output);
```

### Browser (ESM)

```js
import { Preprocessor } from "content-tag";
let p = new Preprocessor();
let output = p.process("<template>Hi</template>");

console.log(output);
```

## API

### `Preprocessor`

All `content-tag` public API lives on the `Preprocessor` object.

### `new Preprocessor(options?: PreprocessorConstructorOptions)`

Creates a preprocessor. By default it reads `<template>` content tags, ends
each one at its first closing tag, and `process` writes the code that Ember
expects.

#### `PreprocessorConstructorOptions`

```ts
interface PreprocessorConstructorOptions {
  /** The content tags to read, by name. Default is `{ template: {} }`. */
  tags?: Record<string, TagOptions>;
}

interface TagOptions {
  /** Returns the index of the closing tag's `<`. Default is the first closing tag. */
  findEnd?(source: string, openTag: OpenTag): number;
  /** Returns the code that `process` writes in place of the tag. */
  transform?(tag: Parsed, context: TransformContext): string;
}

interface OpenTag {
  name: string;
  /** The index of the opening tag's `<`. */
  start: number;
  /** The index just after the opening tag. */
  contentStart: number;
}

interface TransformContext {
  /** Adds an import and returns the local name to use for it. */
  import(name: string, from: string): string;
}
```

#### Tags

`tags` lists the content tags to read, by name. The opening tag must be
exactly `<name>`.

```ts
let p = new Preprocessor({ tags: { template: {}, style: {} } });
p.parse("let a = <template>x</template>; let b = <style>y</style>;");
// tagName "template" with contents "x", then tagName "style" with contents "y"
```

#### `findEnd`

Without `findEnd`, a content tag ends at the first closing tag with its name.
That is wrong when the content can contain that closing tag, for example
inside a comment or a nested element. Code that understands the content's
language can give the right end with `findEnd`.

```ts
let p = new Preprocessor({
  tags: {
    template: {
      findEnd(source, openTag) {
        // Read the content from openTag.contentStart, and return the index of
        // the `<` of the closing tag.
      },
    },
  },
});
```

- Indexes are UTF-16 code units, as in any JavaScript string.
- `findEnd` must return the index of a closing tag with the open tag's name,
  at or after `contentStart`. Anything else is a parse error.
- To report an error, throw an `Error`. If it has a numeric `offset`, the
  parse error points at that index. If its `code` is `"missing-closing-tag"`,
  the parse error points at the opening tag.

#### `transform`

By default, `process` writes the code that Ember expects for `<template>`, and
throws for a tag with another name. Give a tag a `transform` to write other
code.

```js
const p = new Preprocessor({
  tags: {
    template: {
      transform(tag, context) {
        const template = context.import("template", "@ember/template-compiler");
        return `${template}(${JSON.stringify(tag.contents)})`;
      },
    },
  },
});

p.process("export const Hi = <template>Hi</template>;");
// code:
// import { template as template_fd9b… } from "@ember/template-compiler";
// export const Hi = template_fd9b…("Hi");
```

`transform` is given the tag as `parse` returns it, and returns the code that
replaces it. Use `tag.type` to return a class member for a tag in a class
body.

- `process` copies the rest of the source as it is. It does not parse or print
  the code that a `transform` returns.
- `context.import(name, from)` adds `import { name } from "from"` to the top of
  the file, once, and returns the local name to use. The local name has a
  suffix so it cannot clash with a name in the file. A `name` of `"default"`
  is the default export. A file with no content tags gets no imports.
- When any tag has a `transform`, every tag needs one.
- `process` also returns `tags`, the tags it replaced with an `outputRange`
  that says where each replacement is in `code`, and `errors`.
- With the `recover` option, `process` does not throw for a syntax error. It
  returns the error in `errors` and still replaces the tags, which it finds
  without parsing the file, as `parseWithRecovery` does.
- The source map has an entry for each word of the copied source. Each
  replacement has one entry, which points at the start of its tag.

### `Preprocessor.process(src: string, options?: PreprocessorOptions): { code: string, map: string };`

Parses a given source code string using the `content-tag` spec into standard
JavaScript.

```ts
import { Preprocessor } from "content-tag";
let p = new Preprocessor();
let output = p.process("<template>Hi</template>");
```

### `Preprocessor.parse(src: string, options?: PreprocessorOptions): Parsed[];`

Parses a given source code string using the `content-tag` spec into an array of
`Parsed` content tag objects.

```ts
import { Preprocessor } from "content-tag";
let p = new Preprocessor();
let output = p.parse("<template>Hi</template>");
```

### `Preprocessor.parseWithRecovery(src: string, options?: PreprocessorOptions): Recovered;`

Like `parse`, but it also finds the content tags in a file that has a syntax
error, and does not throw for one. This is for tools such as editors, which
see files while they are being typed.

```ts
let p = new Preprocessor();
let { templates, errors } = p.parseWithRecovery(
  "const T = <template>Hi</template>;\nconst a =;",
);
// templates: one `Parsed`, for "Hi"
// errors: [{ kind: "syntax", message, range }] for the syntax error
```

```ts
interface Recovered {
  templates: Parsed[];
  /** Empty when the file parsed. The parse error comes first. */
  errors: {
    kind: "syntax" | "missing-closing-tag" | "tag-content";
    message: string;
    range: Range;
  }[];
}
```

Each error has a `kind`:

- `"syntax"` means the code outside the content tags does not parse.
- `"missing-closing-tag"` means a content tag has no closing tag. Its `range`
  is the opening tag. The tag then ends at the first closing tag after it,
  which belongs to another tag, or it is left out of `templates` when there
  is none.
- `"tag-content"` means `findEnd` could not read the content of a tag, such
  as a comment in it that is not closed. The tag ends at its first closing
  tag.

When the file parses, `templates` is the same as `parse` gives. When it does
not, swc's parser gives no syntax tree, so the content tags are found with
swc's lexer alone, using the same tags and `findEnd`. Those templates have
the same ranges and contents, with two limits:

- `type` is a guess. A content tag is reported as `"class-member"` when it is
  directly inside the body of a `class` and starts a member. It does not start
  one after a token that needs a value, so the tag in `x = <template>` is an
  `"expression"`, as is any other.
- A regex in an unusual place can be read as a division, so its content is
  read as code. One such place is straight after the block of a `case x:` or
  a label.

When `findEnd` throws or gives a bad index, the content tag ends at its first
closing tag instead, and the error is added to `errors`.

#### `PreprocessorOptions`

```ts
interface PreprocessorOptions {
  /** Default is `false` */
  inline_source_map?: boolean;

  filename?: string;

  /**
   * Only for `process` with a `transform`. Return a syntax error in `errors`
   * and do not throw. Default is `false`.
   */
  recover?: boolean;
}
```

#### `Parsed`

````ts
interface Range {
  // Range in raw bytes.
  startByte: number;
  endByte: number;

  // Range in unicode codepoints.
  // CAUTION: see "Unicode Codepoint Slicing Warning" below.
  startChar: number;
  endChar: number;

  // utf16 is used by JavaScript strings.
  // e.g. str.slice(range.startUtf16Codepoint, range.endUtf16Codepoint)
  startUtf16Codepoint: number;
  endUtf16Codepoint: number;
}

interface Parsed {
  /**
   * The type for the content tag.
   *
   * 'expression' corresponds to a tag in an expression position, e.g.
   * ```
   * const HiComponent = <template>Hi</template>;
   * ```
   *
   * 'class-member' corresponds to a tag in a class-member position, e.g.
   * ```
   * export default class HiComponent extends Component {
   *   <template>Hi</template>
   * }
   * ```
   */
  type: "expression" | "class-member";

  /**
   * The content tag's name, such as `"template"`.
   */
  tagName: string;

  /** Raw template contents. */
  contents: string;

  /**
   * Range of the contents, inclusive of the
   * `<template></template>` tags.
   */
  range: Range;

  /**
   * Range of the template contents, not inclusive of the
   * `<template></template>` tags.
   */
  contentRange: {
    start: number;
    end: number;
  };

  /**
   * Range of the opening `<template>` tag.
   */
  startRange: Range;

  /**
   * Range of the closing `</template>` tag.
   */
  endRange: Range;
}
````

## Unicode Codepoint Slicing Warning

If you have a string and want to use the range provided by our `parse` method to slice out parts of that string, you need avoid two major pitfalls.

First, you want to use our `startChar` and `endChar` not `startByte` and `endByte`. Earlier versions of this library only provided `start` and `end` and they were always bytes, making string slicing unnecessarily difficult.

Second, beware that Javascript's `String.prototype.slice` doesn't actually work on Unicode codepoints. It works on UTF-16 units, which are not the same thing. Intead, you can rely on `String.prototype[Symbol.iterator]` which _does_ work on Unicode codepoints. So this is safe, even when fancy things like emojis are present:

```js
Array.from(myString).slice(range.startChar, range.endChar).join("");
```

if you want to just slice from a string you can use 
```js
str.slice(range.startUtf16Codepoint, range.endUtf16Codepoint)
```

## Contributing

See the [CONTRIBUTING.md](./CONTRIBUTING.md) file.
