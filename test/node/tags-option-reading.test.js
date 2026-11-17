import chai from "chai";
import { Preprocessor } from "content-tag";

const { expect } = chai;

const contents = (parsed) => parsed.map((o) => [o.tagName, o.contents]);

// Ends each content tag at its last closing tag in the source.
const lastClosingTag = {
  template: {
    findEnd(source, openTag) {
      return source.lastIndexOf(`</${openTag.name}>`);
    },
  },
};

describe("tags option: reading tags", function () {
  it("reads <template> when no options are given", function () {
    const output = new Preprocessor().parse("<template>hi</template>");
    expect(contents(output)).to.eql([["template", "hi"]]);
  });

  it("reads the tags that are listed", function () {
    const p = new Preprocessor({ tags: { template: {}, style: {} } });
    const output = p.parse(
      "let a = <template>x</template>; let b = <style>y</style>;",
    );
    expect(contents(output)).to.eql([
      ["template", "x"],
      ["style", "y"],
    ]);
  });

  it("does not read tags that are not listed", function () {
    const p = new Preprocessor({ tags: { jsx: {} } });
    let output;
    try {
      output = p.parse("let x = <template>hi</template>");
    } catch {
      output = [];
    }
    expect(output).to.eql([]);
  });

  it("ends at the first closing tag without findEnd", function () {
    const p = new Preprocessor();
    expect(() =>
      p.parse("class A { <template><template>a</template></template> }"),
    ).to.throw(/Parse Error/);
  });

  it("lets findEnd decide where the content ends", function () {
    const p = new Preprocessor({ tags: lastClosingTag });
    const output = p.parse(
      `class A { <template><template id="row">a</template></template> }`,
    );
    expect(contents(output)).to.eql([
      ["template", `<template id="row">a</template>`],
    ]);
  });

  it("gives findEnd the source and the open tag in UTF-16 indexes", function () {
    const calls = [];
    const p = new Preprocessor({
      tags: {
        template: {
          findEnd(source, openTag) {
            calls.push([source, { ...openTag }]);
            return source.indexOf("</template>", openTag.contentStart);
          },
        },
      },
    });
    const src = "let ü = '💩';\nlet x = <template>💩</template>";
    const output = p.parse(src);
    expect(calls).to.eql([
      [src, { name: "template", start: 22, contentStart: 32 }],
    ]);
    expect(src.slice(22, 32)).to.equal("<template>");
    expect(contents(output)).to.eql([["template", "💩"]]);
    expect(output[0].contentRange.startUtf16Codepoint).to.equal(32);
  });

  it("uses findEnd in process too", function () {
    const p = new Preprocessor({ tags: lastClosingTag });
    const output = p.process(
      "let x = <template><template>a</template></template>",
    );
    expect(output.code).to.match(/`<template>a<\/template>`/);
  });

  it("reports an error thrown by findEnd at its offset", function () {
    const p = new Preprocessor({
      tags: {
        template: {
          findEnd() {
            throw Object.assign(new Error("This comment is not closed"), {
              offset: 12,
            });
          },
        },
      },
    });
    expect(() => p.parse("let x = 1;\n <template>{{!-- </template>")).to.throw(
      /Parse Error at <anon>:2:2: 2:2/,
    );
    try {
      p.parse("let x = 1;\n <template>{{!-- </template>");
    } catch (err) {
      expect(err.source_code).to.match(/This comment is not closed/);
    }
  });

  it("reports an error thrown by findEnd without an offset", function () {
    const p = new Preprocessor({
      tags: {
        template: {
          findEnd() {
            throw new Error("nope");
          },
        },
      },
    });
    try {
      p.parse("<template>a</template>");
      expect.fail("parse should throw");
    } catch (err) {
      expect(err.source_code).to.match(/nope/);
    }
  });

  it("rejects a findEnd result that is not a closing tag", function () {
    for (const result of [3, -1, 1.5, "11", 1000]) {
      const p = new Preprocessor({
        tags: { template: { findEnd: () => result } },
      });
      expect(
        () => p.parse("<template>a</template>"),
        `findEnd returning ${result}`,
      ).to.throw(/Parse Error/);
    }
  });

  it("throws for invalid tags options", function () {
    expect(() => new Preprocessor({ tags: "x" })).to.throw(
      TypeError,
      /`tags` option must be an object/,
    );
    expect(() => new Preprocessor({ tags: { template: 1 } })).to.throw(
      TypeError,
      /`tags.template` option must be an object/,
    );
    expect(() => new Preprocessor({ tags: { a: { findEnd: 1 } } })).to.throw(
      TypeError,
      /`tags.a.findEnd` option must be a function/,
    );
    expect(() => new Preprocessor({ tags: {} })).to.throw(
      /must list at least one tag/,
    );
    expect(() => new Preprocessor({ tags: { "my tag": {} } })).to.throw(
      /Invalid content tag name "my tag"/,
    );
  });
});
