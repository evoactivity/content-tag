import chai from "chai";
import { codeEquality } from "code-equality-assertions/chai";
import { Preprocessor } from "content-tag";

chai.use(codeEquality);

const { expect } = chai;

const SUFFIX = "fd9b2463e5f141cfb5666b64daa1f11a";

// A preprocessor for `<template>` with one output.
function withOutput(output) {
  return new Preprocessor({ output: { template: output } });
}

describe("output option", function () {
  it("replaces expressions and class members", function () {
    const p = new Preprocessor({
      parser: { tags: ["jsx"] },
      output: {
        jsx: {
          import: 'import compiler from "thing";',
          expression: "compiler(`___CT_CONTENT___`)",
          classMember: "static template = compiler(`___CT_CONTENT___`);",
        },
      },
    });
    const output = p.process(
      "let x = <jsx>hi</jsx>; class A { <jsx>yo</jsx> }",
    );
    expect(output.code).to.equalCode(
      `import compiler_${SUFFIX} from "thing";
       let x = compiler_${SUFFIX}(\`hi\`);
       class A { static template = compiler_${SUFFIX}(\`yo\`); }`,
    );
  });

  it("uses a static block when classMember is not given", function () {
    const p = withOutput({ import: "", expression: "c(`___CT_CONTENT___`)" });
    expect(p.process("class A { <template>yo</template> }").code).to.equalCode(
      "class A { static { c(`yo`); } }",
    );
  });

  it("can keep indentation and skip export default", function () {
    const p = withOutput({
      import: "",
      expression: "c(`___CT_CONTENT___`)",
      stripIndent: false,
      exportDefault: false,
    });
    expect(p.process("<template>\n  hi\n</template>").code).to.equalCode(
      "c(`\n  hi\n`);",
    );
  });

  it("replaces a quoted placeholder with a string", function () {
    const p = withOutput({ import: "", expression: 'c("___CT_CONTENT___")' });
    expect(p.process("let x = <template>hi</template>").code).to.equalCode(
      'let x = c("hi");',
    );
  });

  it("replaces a bare placeholder with the content as code", function () {
    const p = new Preprocessor({
      parser: { tags: ["jsx"] },
      output: { jsx: { import: "", expression: "c(___CT_CONTENT___)" } },
    });
    expect(p.process("let x = <jsx>{ a: 1 }</jsx>").code).to.equalCode(
      "let x = c({ a: 1 });",
    );
  });

  it("reports content that does not parse as code", function () {
    const p = new Preprocessor({
      parser: { tags: ["jsx"] },
      output: { jsx: { import: "", expression: "c(___CT_CONTENT___)" } },
    });
    expect(() => p.process("let x = <jsx>a b</jsx>")).to.throw(/Parse Error/);
  });

  it("removes class member tags when classMember is empty", function () {
    const p = withOutput({
      import: "",
      expression: "c(`___CT_CONTENT___`)",
      classMember: "",
    });
    expect(
      p.process("class A { a = 1; <template>hi</template> }").code,
    ).to.equalCode("class A { a = 1; }");
  });

  it("allows an expression without a placeholder", function () {
    const p = withOutput({ import: "", expression: "undefined" });
    expect(p.process("let x = <template>hi</template>").code).to.equalCode(
      "let x = undefined;",
    );
  });

  it("uses each tag's own output", function () {
    const p = new Preprocessor({
      parser: { tags: ["template", "style"] },
      output: {
        template: { import: "", expression: "t(`___CT_CONTENT___`)" },
        style: { import: "", expression: 's("___CT_CONTENT___")' },
      },
    });
    expect(
      p.process("let a = <template>x</template>; let b = <style>y</style>;")
        .code,
    ).to.equalCode('let a = t(`x`); let b = s("y");');
  });

  it("throws in process for a tag with no output", function () {
    const p = new Preprocessor({
      parser: { tags: ["template", "style"] },
      output: { template: { import: "", expression: "t(`___CT_CONTENT___`)" } },
    });
    expect(() => p.process("let b = <style>y</style>;")).to.throw(
      /Parse Error/,
    );
    expect(p.parse("let b = <style>y</style>;")).to.have.length(1);
  });

  it("throws for an output for a tag the parser does not read", function () {
    expect(
      () =>
        new Preprocessor({
          output: { style: { import: "", expression: "s()" } },
        }),
    ).to.throw(/There is an output for `style`/);
  });

  it("throws when import or expression is missing", function () {
    expect(() => withOutput({ expression: "c(`___CT_CONTENT___`)" })).to.throw(
      TypeError,
      /`output.template.import` option is required/,
    );
    expect(() => withOutput({ import: "" })).to.throw(
      TypeError,
      /`output.template.expression` option is required/,
    );
  });

  it("throws when an option has the wrong type", function () {
    expect(() => new Preprocessor({ output: "c" })).to.throw(
      TypeError,
      /`output` option must be an object/,
    );
    expect(() => withOutput("c")).to.throw(
      TypeError,
      /`output.template` option must be an object/,
    );
    expect(() =>
      withOutput({
        import: "",
        expression: "c(`___CT_CONTENT___`)",
        stripIndent: 1,
      }),
    ).to.throw(
      TypeError,
      /`output.template.stripIndent` option must be a boolean/,
    );
  });

  it("throws when a bare placeholder is not an expression", function () {
    expect(() =>
      withOutput({ import: "", expression: "(___CT_CONTENT___) => 1" }),
    ).to.throw(
      /Invalid `output.expression` option. ___CT_CONTENT___ can only be used as an expression/,
    );
  });

  it("throws when the import is not only import declarations", function () {
    expect(() =>
      withOutput({ import: "c();", expression: "c(`___CT_CONTENT___`)" }),
    ).to.throw(/Invalid `output.import` option/);
  });
});
