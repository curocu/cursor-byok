import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import test from "node:test";

globalThis.t = (text) => text;
const source = stripTypeScriptTypes(await readFile(new URL("./imageGeneration.ts", import.meta.url), "utf8"));
const { readImageOptions, writeImageOptions, imageOptionsError, migrateImageOptions, imageModelLabel } = await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`);

test("image options use the Images root or Responses tool and preserve unknown fields", () => {
  for (const endpoint of ["/v1/images/generations", "responses-image-generation"]) {
    const extra = endpoint === "/v1/images/generations"
      ? { size: "2048x2048", custom: { enabled: true } }
      : { custom: { enabled: true }, tools: [{ type: "image_generation", model: "separate-image-model", size: "2048x2048", custom_tool: 12 }] };
    const original = structuredClone(extra);
    const next = writeImageOptions(extra, endpoint, { size: "1536x864", quality: "high", output_format: "webp", output_compression: 75 });
    assert.deepEqual(extra, original);
    assert.deepEqual(next.custom, extra.custom);
    assert.equal(readImageOptions(next, endpoint).size, "1536x864");
    assert.equal(readImageOptions(next, endpoint).output_compression, 75);
    if (endpoint === "responses-image-generation") {
      assert.equal(next.size, undefined);
      assert.equal(next.tools[0].model, "separate-image-model");
      assert.equal(next.tools[0].custom_tool, 12);
    }
    const cleared = writeImageOptions(next, endpoint, { size: undefined });
    assert.equal(readImageOptions(cleared, endpoint).size, undefined);
    assert.equal(readImageOptions(cleared, endpoint).quality, "high");
  }
});

test("malformed tool definitions are never overwritten by dropdown edits", () => {
  for (const tools of [{}, [], [{ type: "web_search" }], [{ type: "image_generation" }, { type: "web_search" }]]) {
    assert.throws(() => writeImageOptions({ tools }, "responses-image-generation", { size: "auto" }));
  }
});

test("compatible custom dimensions survive and unsupported parameter combinations are blocked", () => {
  assert.equal(imageOptionsError({ size: "1536x864", output_format: "webp", background: "transparent", output_compression: 0 }), null);
  assert.equal(imageOptionsError({}), null);
  for (const value of [{ size: "bogus" }, { size: "0x1024" }, { output_format: "jpeg", background: "transparent" },
    { output_format: "png", output_compression: 90 }, { output_compression: 101 }, { output_compression: -1 }]) {
    assert.ok(imageOptionsError(value));
  }
});

test("switching image protocol moves known options and preserves unrelated fields", () => {
  const images = {
    size: "1536x864",
    quality: "high",
    background: "opaque",
    private_root: { keep: true },
  };
  const responses = migrateImageOptions(images, "/v1/images/generations", "responses-image-generation");
  assert.equal(responses.size, undefined);
  assert.equal(responses.quality, undefined);
  assert.equal(responses.background, undefined);
  assert.deepEqual(responses.private_root, { keep: true });
  assert.deepEqual(responses.tools, [{ type: "image_generation", size: "1536x864", quality: "high", background: "opaque" }]);

  const back = migrateImageOptions({ ...responses, private_tool: 1, tools: [{ ...responses.tools[0], model: "image-model" }] }, "responses-image-generation", "/v1/images/generations");
  assert.equal(back.tools, undefined);
  assert.equal(back.size, "1536x864");
  assert.equal(back.quality, "high");
  assert.equal(back.background, "opaque");
  assert.deepEqual(back.private_root, { keep: true });
  assert.equal(back.model, undefined);

  const alreadyResponses = { tools: [{ type: "image_generation", quality: "low", tool_private: "keep" }], root_private: 2 };
  const moved = migrateImageOptions(alreadyResponses, "/v1/images/generations", "responses-image-generation");
  assert.equal(moved.tools[0].quality, "low");
  assert.equal(moved.tools[0].tool_private, "keep");
  assert.equal(moved.root_private, 2);

  assert.throws(() => migrateImageOptions({ tools: [{ type: "web_search" }] }, "responses-image-generation", "/v1/images/generations"));
  assert.deepEqual(migrateImageOptions({ size: "1024x1024" }, "/v1/images/generations", "/v1/images/generations"), { size: "1024x1024" });
});

test("same-name image models stay distinguishable in pickers", () => {
  const first = imageModelLabel({ display_name: "Image", model_id: "dall-e-3", openai_endpoint: "/v1/images/generations" });
  const second = imageModelLabel({ display_name: "Image", model_id: "gpt-image-1", openai_endpoint: "responses-image-generation" });
  assert.notEqual(first, second);
  assert.match(first, /dall-e-3/);
  assert.match(second, /gpt-image-1/);
  assert.match(second, /Responses/);
});
