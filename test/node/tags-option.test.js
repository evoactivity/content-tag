import chai from "chai";
import { Preprocessor } from "content-tag";

const { expect } = chai;

// Reads a source map's mappings as `[generatedLine, generatedColumn,
// sourceLine, sourceColumn]`, all starting at 0.
function mappings(map) {
  const base64 =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const decode = (segment) => {
    const values = [];
    let value = 0;
    let shift = 0;
    for (const char of segment) {
      const digit = base64.indexOf(char);
      value |= (digit & 31) << shift;
      if (digit & 32) {
        shift += 5;
      } else {
        values.push(value & 1 ? -(value >> 1) : value >> 1);
        value = 0;
        shift = 0;
      }
    }
    return values;
  };
  const result = [];
  let sourceLine = 0;
  let sourceColumn = 0;
  JSON.parse(map)
    .mappings.split(";")
    .forEach((line, generatedLine) => {
      let generatedColumn = 0;
      for (const segment of line.split(",")) {
        if (!segment) continue;
        const [column, , lineDelta, columnDelta] = decode(segment);
        generatedColumn += column;
        sourceLine += lineDelta;
        sourceColumn += columnDelta;
        result.push([generatedLine, generatedColumn, sourceLine, sourceColumn]);
      }
    });
  return result;
}

const mark = (tag) =>
  tag.type === "class-member"
    ? `[${tag.tagName}:${tag.contents}]`
    : `<${tag.tagName}:${tag.contents}>`;

describe("tags option", function () {
  it("replaces each tag with what its transform returns", function () {
    const p = new Preprocessor({ tags: { template: { transform: mark } } });
    const src =
      "class A {\n  <template>a</template>\n}\nlet  x = <template>b</template> ;";
    const { code, tags, errors } = p.process(src);
    // The rest of the source is copied as it is.
    expect(code).to.equal(
      "class A {\n  [template:a]\n}\nlet  x = <template:b> ;",
    );
    expect(errors).to.eql([]);
    expect(tags.map((tag) => [tag.type, tag.contents])).to.eql([
      ["class-member", "a"],
      ["expression", "b"],
    ]);
  });

  it("gives each transform the tag as parse returns it", function () {
    const seen = [];
    const p = new Preprocessor({
      tags: {
        template: {
          transform(tag) {
            seen.push(tag);
            return "x";
          },
        },
      },
    });
    const src = "let ü = <template>💩</template>;";
    p.process(src);
    expect(seen).to.eql(new Preprocessor().parse(src));
  });

  it("says where each replacement is in the output", function () {
    const p = new Preprocessor({ tags: { template: { transform: mark } } });
    const { code, tags } = p.process(
      "let ü = <template>a</template>;\nlet y = <template>b</template>;",
    );
    expect(
      tags.map((tag) =>
        code.slice(
          tag.outputRange.startUtf16Codepoint,
          tag.outputRange.endUtf16Codepoint,
        ),
      ),
    ).to.eql(["<template:a>", "<template:b>"]);
  });

  it("adds each import once, with a name that cannot clash", function () {
    const p = new Preprocessor({
      tags: {
        template: {
          transform(tag, { import: add }) {
            const template = add("template", "@ember/template-compiler");
            return `${template}(\`${tag.contents}\`)`;
          },
        },
      },
    });
    const { code } = p.process(
      "let template = 1;\nlet x = <template>a</template>;\nlet y = <template>b</template>;",
    );
    const [first, ...rest] = code.split("\n");
    const local =
      /^import \{ template as (template_\w+) \} from "@ember\/template-compiler";$/.exec(
        first,
      )?.[1];
    expect(local, first).to.be.a("string");
    expect(rest).to.eql([
      "let template = 1;",
      `let x = ${local}(\`a\`);`,
      `let y = ${local}(\`b\`);`,
    ]);
  });

  it("adds no imports to a file with no tags", function () {
    const p = new Preprocessor({
      tags: { template: { transform: (tag, ctx) => ctx.import("a", "b") } },
    });
    expect(p.process("let x = 1;").code).to.equal("let x = 1;");
  });

  it("uses each tag's own findEnd and transform", function () {
    const p = new Preprocessor({
      tags: {
        template: {
          findEnd: (source) => source.lastIndexOf("</template>"),
          transform: (tag) => `T(${JSON.stringify(tag.contents)})`,
        },
        style: { transform: (tag) => `S(${JSON.stringify(tag.contents)})` },
      },
    });
    const { code } = p.process(
      "let a = <style>p{}</style>;\nlet b = <template>x</template>y</template>;",
    );
    expect(code).to.equal('let a = S("p{}");\nlet b = T("x</template>y");');
  });

  it("throws for a syntax error without recover", function () {
    const p = new Preprocessor({ tags: { template: { transform: mark } } });
    expect(() => p.process("let x = ;")).to.throw(/Parse Error/);
  });

  it("returns a syntax error and still replaces the tags with recover", function () {
    const p = new Preprocessor({ tags: { template: { transform: mark } } });
    const { code, errors } = p.process(
      "let x = <template>a</template>;\nconst a =;\nlet y = <template>b</template>;",
      { recover: true },
    );
    expect(code).to.equal(
      "let x = <template:a>;\nconst a =;\nlet y = <template:b>;",
    );
    expect(errors.map((error) => error.kind)).to.eql(["syntax"]);
  });

  it("passes on what a transform throws", function () {
    class Mine extends Error {}
    const p = new Preprocessor({
      tags: {
        template: {
          transform() {
            throw new Mine("no");
          },
        },
      },
    });
    expect(() => p.process("let x = <template>a</template>;")).to.throw(Mine);
    // The preprocessor can still be used.
    expect(() => p.process("let x = 1;")).not.to.throw();
  });

  it("needs a transform to return a string", function () {
    const p = new Preprocessor({ tags: { template: { transform: () => 1 } } });
    expect(() => p.process("let x = <template>a</template>;")).to.throw(
      TypeError,
      /must return a string/,
    );
  });

  it("maps the copied source and each replacement to where they came from", function () {
    const p = new Preprocessor({
      tags: {
        template: {
          transform: (tag, ctx) => `${ctx.import("t", "m")}(1,\n2)`,
        },
      },
    });
    const src = "let a = 1;\nlet b = <template>x</template>; let c = 2;";
    const { code, map } = p.process(src, { filename: "a.gjs" });
    expect(JSON.parse(map).sources).to.eql(["a.gjs"]);

    const sourceLines = src.split("\n");
    const codeLines = code.split("\n");
    const word = (lines, line, column) =>
      /^\S+/.exec(lines[line].slice(column))?.[0];
    const found = mappings(map);
    // The import on the first line has no mapping.
    expect(found.every(([line]) => line > 0)).to.equal(true);
    // Each mapping outside the replacement joins the same word.
    const copied = found.filter(
      ([line, column]) => !codeLines[line].slice(column).startsWith("t_"),
    );
    expect(copied.length).to.be.greaterThan(6);
    for (const [line, column, sourceLine, sourceColumn] of copied) {
      expect(word(codeLines, line, column)).to.equal(
        word(sourceLines, sourceLine, sourceColumn),
      );
    }
    // The replacement maps to the start of the tag.
    const replacement = found.find(([line, column]) =>
      codeLines[line].slice(column).startsWith("t_"),
    );
    expect(replacement.slice(2)).to.eql([1, sourceLines[1].indexOf("<")]);
    // `let c` comes after a replacement that has a new line in it.
    expect(found).to.deep.include([3, 4, 1, sourceLines[1].indexOf("let c")]);
  });

  it("uses the Ember output for template when no tag has a transform", function () {
    const p = new Preprocessor({
      tags: {
        template: { findEnd: (source) => source.lastIndexOf("</template>") },
      },
    });
    const { code } = p.process("<template>a</template>b</template>");
    expect(code).to.include("a</template>b");
    expect(code).to.include("@ember/template-compiler");
  });

  it("rejects options that do not fit together", function () {
    expect(
      () =>
        new Preprocessor({
          tags: { template: { transform: mark }, style: {} },
        }),
    ).to.throw(TypeError, /`tags.style` option needs a `transform`/);
    expect(
      () => new Preprocessor({ tags: { template: { transform: 1 } } }),
    ).to.throw(TypeError, /must be a function/);
  });
});
