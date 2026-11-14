import chai from "chai";
import { Preprocessor } from "content-tag";

const { expect } = chai;

const contents = (recovered) =>
  recovered.templates.map((t) => [t.type, t.contents]);

describe("parseWithRecovery", function () {
  it("gives the same templates as parse for a file that parses", function () {
    const p = new Preprocessor();
    const src =
      "class A { <template>a</template> }\nlet x = <template>b</template>;";
    const recovered = p.parseWithRecovery(src);
    expect(recovered.templates).to.eql(p.parse(src));
    expect(recovered.errors).to.eql([]);
  });

  it("finds templates around a syntax error instead of throwing", function () {
    const p = new Preprocessor();
    const src =
      "const T = <template>hi</template>;\nconst a =;\nconst U = <template>yo</template>;";
    expect(() => p.parse(src)).to.throw(/Parse Error/);
    const recovered = p.parseWithRecovery(src);
    expect(contents(recovered)).to.eql([
      ["expression", "hi"],
      ["expression", "yo"],
    ]);
    expect(recovered.errors).to.have.length.of.at.least(1);
    const [error] = recovered.errors;
    expect(error.message).to.be.a("string");
    expect(src.slice(error.range.startUtf16Codepoint)).to.match(/^;/);
  });

  it("finds templates while a call is still being typed", function () {
    const p = new Preprocessor();
    const recovered = p.parseWithRecovery(
      "class A {\n  <template>hi</template>\n}\nfoo(\n",
    );
    expect(contents(recovered)).to.eql([["class-member", "hi"]]);
  });

  it("gives the same ranges as parse for non-ASCII source", function () {
    const p = new Preprocessor();
    const valid = "let ü = '💩';\nclass A { <template>💩</template> }";
    const recovered = p.parseWithRecovery(valid + "\nconst a =;");
    expect(recovered.templates).to.eql(p.parse(valid));
  });

  it("ignores templates in regexes and template literals", function () {
    const p = new Preprocessor();
    const recovered = p.parseWithRecovery(
      "const r = /<template>no<\\/template>/;\nconst s = `${a}<template>no</template>`;\nconst T = <template>hi</template>;\nfoo(",
    );
    expect(contents(recovered)).to.eql([["expression", "hi"]]);
  });

  it("uses findEnd in a broken file", function () {
    const p = new Preprocessor({
      parser: {
        tags: ["template"],
        findEnd(source, openTag) {
          return source.lastIndexOf(`</${openTag.name}>`);
        },
      },
    });
    const recovered = p.parseWithRecovery(
      "let x = <template><template>a</template></template>;\nconst a =;",
    );
    expect(contents(recovered)).to.eql([
      ["expression", "<template>a</template>"],
    ]);
  });

  it("falls back to the first closing tag when findEnd throws", function () {
    const p = new Preprocessor({
      parser: {
        tags: ["template"],
        findEnd() {
          throw new Error("cannot find the end");
        },
      },
    });
    const recovered = p.parseWithRecovery("let x = <template>a</template>;");
    expect(contents(recovered)).to.eql([["expression", "a"]]);
    expect(recovered.errors.map((e) => e.message)).to.include(
      "cannot find the end",
    );
  });
});
