import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import "./register-typescript.mjs";

// Execute source modules in memory, including parameter properties. No dist
// output or Rust/WASM build is required for these scheduling/geometry tests.
registerHooks({ load(url, context, nextLoad) {
  if (!url.endsWith(".ts")) return nextLoad(url, context);
  return { format: "module", shortCircuit: true, source: ts.transpileModule(
    readFileSync(new URL(url), "utf8"),
    { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 } },
  ).outputText };
} });

const { buildSdfAtlas } = await import("../../src/fontSdfAtlas.ts");
const { glyphMagnification, highResolutionPointSize, substituteHighResolutionGlyph,
  refineHighResolutionSdf } = await import("../../src/highResolutionSdf.ts");
const { glyphTexelMap } = await import("../../src/gpu/webglSdfGlyphPipeline.ts");

test("magnification uses the final quad and keeps normal text on its base atlas", () => {
  const base = { baseSize: 75, width: 2048, height: 2048 };
  const glyph = { width: 20, height: 10 };
  assert.equal(glyphMagnification({ drawable: true, quad: [[100, 0], [100, -80], [120, -80], [120, 0]] }, glyph), 4);
  assert.equal(highResolutionPointSize(base, glyph, 3), 75);
  assert.equal(highResolutionPointSize(base, glyph, 3.01), 300);
  assert.equal(highResolutionPointSize(base, glyph, 3.99), 300);
  assert.equal(highResolutionPointSize(base, glyph, 4.01), 375);
  assert.equal(highResolutionPointSize(base, glyph, 7), 525);
  assert.equal(highResolutionPointSize(base, glyph, Infinity), 75);
  const pointSize = highResolutionPointSize(base, { width: 250, height: 180 }, 100);
  assert.ok(pointSize / 75 * 250 <= 2040);
});

test("tier replacement preserves font-space sample alignment, layout and materials", () => {
  const primary = { width: 29, height: 43, planeBearingX: -1.3, planeBearingY: 29.2,
    u0: 10 / 2048, v0: 20 / 2048, u1: 39 / 2048, v1: 63 / 2048, page: 0 };
  const target = { width: 112, height: 168, planeBearingX: -5.1, planeBearingY: 116.9,
    u0: 100 / 2048, v0: 80 / 2048, u1: 212 / 2048, v1: 248 / 2048, page: 2, drawable: true };
  const original = { drawable: true, quad: [
    [100, 70, primary.u0, primary.v0, 3], [216, 40, primary.u1, primary.v0, 3],
    [260, 255, primary.u1, primary.v1, 3], [144, 285, primary.u0, primary.v1, 3],
  ], deviceGlyphQuad: ["A", []], charQuad: ["A", [[1, 2]]], charOp: ["A", 4, 5],
    layoutMetrics: { lineWidths: [42] }, shaderFaceScale: 55, shaderFaceBias: 27,
    shaderUnderlayScale: 22, shaderUnderlayBias: 10, shaderVertexAlpha: 0.7 };
  const refined = substituteHighResolutionGlyph(original, primary, { baseSize: 75, spread: 6 }, target, { baseSize: 300, spread: 24 });
  assert.equal(refined.atlasPage, 2);
  assert.strictEqual(refined.charOp, original.charOp);
  assert.strictEqual(refined.charQuad, original.charQuad);
  assert.strictEqual(refined.layoutMetrics, original.layoutMetrics);
  for (const property of ["shaderFaceScale", "shaderFaceBias", "shaderUnderlayScale", "shaderUnderlayBias", "shaderVertexAlpha"]) {
    assert.equal(refined[property], original[property]);
  }
  const before = glyphTexelMap(original.quad.map(q => q.slice(0, 2)), [10, 20, 29, 43]);
  const after = glyphTexelMap(refined.quad.map(q => q.slice(0, 2)), [100, 80, 112, 168]);
  const at = (m, x, y) => [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]];
  const left = (g, s) => Math.floor(g.planeBearingX) - s + 0.5;
  const top = (g, s) => Math.ceil(g.planeBearingY) + s - 0.5;
  for (const [x, y] of [[120, 80], [170, 130], [200, 200]]) {
    const [bx, by] = at(before, x, y);
    const [hx, hy] = at(after, x, y);
    assert.ok(Math.abs((hx - 100 + left(target, 24)) - 4 * (bx - 10 + left(primary, 6))) < 0.001);
    assert.ok(Math.abs((top(target, 24) - (hy - 80)) - 4 * (top(primary, 6) - (by - 20))) < 0.001);
  }
});

function fixture({ failSize, failSizes, cancel, failStatsAt, atlasHardPages = 16 } = {}) {
  const sourceHash = "11".repeat(32);
  const fonts = [{ region: "cn", family: "Synthetic", sourceHash }];
  const requests = ["A", "B"].map(char => ({ region: "cn", family: "Synthetic", fontSourceHash: sourceHash, char }));
  const records = new Map();
  const calls = { pools: 0, attempted: [], generated: [], leases: [], released: [] };
  const stats = () => ({ pages: 2, atlasBytes: 2 * 2048 ** 2, pinnedPages: 2, evictions: 0 });
  const atlas = {
    initialStats: stats(),
    async resolve(keys, cached, generate) {
      for (const value of cached) records.set(value.key, value);
      const generated = [];
      for (const group of generate) {
        calls.attempted.push({ pointSize: group.pointSize ?? 75 });
        if ((failSize != null && group.pointSize === failSize) || failSizes?.has(group.pointSize)) {
          throw new Error("MEMORY_BUDGET_EXCEEDED: test pool is full");
        }
        for (const glyph of group.glyphs) {
          if (records.has(glyph.key)) continue;
          const r = (group.pointSize ?? 75) / 75;
          const record = { key: glyph.key, glyphIndex: glyph.glyphIndex,
            width: 20 * r, height: 30 * r, advance: 10 * r,
            xOffset: -6 * r, yOffset: -24 * r,
            planeBearingX: 0, planeBearingY: 18 * r, planeWidth: 8 * r, planeHeight: 18 * r,
            drawable: true, pixels: new Uint8Array(20 * 30 * r * r) };
          records.set(record.key, record);
          generated.push(record);
          calls.generated.push({ char: glyph.char, pointSize: group.pointSize ?? 75 });
        }
      }
      const lease = calls.leases.length + 1;
      calls.leases.push(lease);
      if (generate.some(group => group.pointSize != null)) cancel?.();
      return { generated, placements: keys.map(key => ({ key, placement: {
        page: records.get(key).width > 20 ? 1 : 0, pageEpoch: 1,
        u0: 0, v0: 0, u1: records.get(key).width / 2048, v1: records.get(key).height / 2048,
      } })), leases: [lease], stats: stats() };
    },
    async pages() { return []; },
    async release(lease) { calls.released.push(lease); },
  };
  let statsCalls = 0;
  const worker = {
    async createAtlas(options) { calls.pools++; calls.atlasOptions = options; return atlas; },
    async stats() {
      if (++statsCalls === failStatsAt) throw new Error("stats read failed");
      return { wasmMs: 1 };
    },
    async planGlyphs(request) {
      const size = request.pointSize ?? 75;
      const contractId = `test-edt-${size}`;
      return { region: request.region, family: request.family, fontSourceHash: request.sourceHash,
        backend: "edt", supersample: 2, baseSize: size, spread: size * 6 / 75,
        atlasWidth: 2048, atlasHeight: 2048, fontEngineFingerprint: "freetype-test", contractId,
        glyphs: request.chars.map(ch => ({ ch, glyphIndex: ch.codePointAt(0), identity: {
          opaqueKey: createHash("sha256").update(`${ch}:${size}`).digest("hex"),
          schemaNamespace: "allium.glyph-raster-cache.v1", fontEngineFingerprint: "freetype-test", rasterContractId: contractId,
        } })), missing: [] };
    },
  };
  const options = { worker, fonts, persistence: "memory-only", atlasHardPages };
  const build = () => buildSdfAtlas(fonts, requests, options);
  const instance = (base, ch, scale) => {
    const glyph = [...base.glyphs.values()].find(glyph => glyph.char === ch);
    const w = glyph.width * scale, h = glyph.height * scale;
    return { layerId: ch, glyphKey: glyph.key, char: ch, drawable: true, atlasPage: 0,
      quad: [[0, 0, glyph.u0, glyph.v0, 1], [w, 0, glyph.u1, glyph.v0, 1],
        [w, h, glyph.u1, glyph.v1, 1], [0, h, glyph.u0, glyph.v1, 1]],
      deviceGlyphQuad: [ch, []], shaderVertexAlpha: 1 };
  };
  const operations = ["A", "B"].map(id => ({ command: { id }, visible: true, commandVisible: true }));
  return { calls, build, instance, operations, options };
}

test("only large glyphs get refined; repeated instances and requests reuse one bounded pool", async () => {
  const f = fixture();
  const base = await f.build();
  const small = f.instance(base, "A", 1);
  const refined = await refineHighResolutionSdf(base, [small, f.instance(base, "A", 4), f.instance(base, "A", 7)], f.operations, f.options);
  assert.strictEqual(refined.instances[0], small);
  assert.equal(refined.instances[1].atlasPage, 1);
  assert.equal(f.calls.pools, 1);
  assert.deepEqual(f.calls.generated, [{ char: "A", pointSize: 75 }, { char: "B", pointSize: 75 }, { char: "A", pointSize: 525 }]);
  await refined.atlas.release();
  await refined.atlas.release();
  assert.deepEqual(f.calls.released, [1, 2]);
  const warm = await f.build();
  const again = await refineHighResolutionSdf(warm, [f.instance(warm, "A", 7)], f.operations, f.options);
  assert.equal(f.calls.generated.length, 3);
  assert.equal(f.calls.pools, 1);
  await again.atlas.release();
  assert.equal(new Set(f.calls.released).size, f.calls.leases.length);
});

test("a full atlas keeps base glyphs usable and releases every acquired tier", async () => {
  const f = fixture({ failSize: 750 });
  const base = await f.build();
  const b = f.instance(base, "B", 10);
  const info = console.info;
  const logs = [];
  console.info = message => logs.push(message);
  let result;
  try { result = await refineHighResolutionSdf(base, [f.instance(base, "A", 4), b], f.operations, f.options); }
  finally { console.info = info; }
  assert.equal(result.instances[0].atlasPage, 1);
  assert.equal(result.instances[1].atlasPage, 1);
  assert.match(logs[0], /requested=2 upgraded=2 downgraded=1 fallback=0 atlas_pages=2\/16/);
  assert.deepEqual(f.calls.atlasOptions, { hardPages: 16 });
  await result.atlas.release();
  assert.deepEqual(f.calls.released, f.calls.leases);
});

test("an unplaceable glyph is skipped after one downgrade and later glyphs still refine", async () => {
  const f = fixture({ failSizes: new Set([1200, 1125]) });
  const base = await f.build();
  const large = f.instance(base, "A", 16);
  const info = console.info;
  const logs = [];
  console.info = message => logs.push(message);
  let result;
  try {
    result = await refineHighResolutionSdf(base,
      [large, f.instance(base, "B", 8)], f.operations, f.options);
  } finally {
    console.info = info;
  }
  assert.strictEqual(result.instances[0], large);
  assert.equal(result.instances[1].atlasPage, 1);
  assert.match(logs[0], /requested=2 upgraded=1 downgraded=0 fallback=1/);
  assert.deepEqual(f.calls.attempted.slice(1), [
    { pointSize: 1200 }, { pointSize: 1125 }, { pointSize: 600 },
  ]);
  await result.atlas.release();
});

test("cancellation releases extra tiers while leaving base ownership to the caller", async () => {
  const controller = new AbortController();
  const f = fixture({ cancel: () => controller.abort(new Error("cancelled")) });
  const base = await f.build();
  await assert.rejects(refineHighResolutionSdf(base, [f.instance(base, "A", 5)], f.operations,
    { ...f.options, signal: controller.signal }), /cancelled/);
  assert.deepEqual(f.calls.released, [2]);
  await base.release();
  assert.deepEqual(f.calls.released, [2, 1]);
});

test("prebuilt atlases and hidden glyphs never allocate high-resolution pages", async () => {
  const f = fixture();
  const base = await f.build();
  const instances = [f.instance(base, "A", 8)];
  const hidden = f.operations.map(op => ({ ...op, visible: false }));
  assert.strictEqual((await refineHighResolutionSdf(base, instances, hidden, f.options)).atlas, base);
  const prebuilt = { ...base, contractId: "prebuilt:test" };
  assert.strictEqual((await refineHighResolutionSdf(prebuilt, instances, f.operations, f.options)).atlas, prebuilt);
  assert.equal(f.calls.pools, 1);
  assert.equal(f.calls.generated.length, 2);
  await base.release();
});

test("a failed post-generation readback releases its acquired tier lease", async () => {
  const f = fixture({ failStatsAt: 4 });
  const base = await f.build();
  await assert.rejects(refineHighResolutionSdf(base, [f.instance(base, "A", 5)], f.operations, f.options), /stats read failed/);
  assert.deepEqual(f.calls.released, [2]);
  await base.release();
  assert.deepEqual(f.calls.released, [2, 1]);
});

test("sampled WASM exports are anchored and included in the browser linker", () => {
  const source = file => readFileSync(new URL(`../../${file}`, import.meta.url), "utf8");
  for (const name of ["sdf_layout_freetype_plan_glyphs_at_size_json", "sdf_layout_freetype_build_glyphs_at_size_json"]) {
    assert.ok(source("build.sh").includes(`'_${name}'`));
    assert.ok(source("src/main.rs").includes(`exports::${name} as *const ()`));
    assert.ok(source("src/lib.rs").includes(`pub unsafe extern "C" fn ${name}(`));
    assert.ok(source("src/worker.ts").includes(`"${name}"`));
  }
});

test("worker preserves legacy ABI arguments and passes sampled size after supersampling", async () => {
  const heap = new Uint8Array(4096);
  heap.set(new TextEncoder().encode('{"glyphs":[]}\0'), 2048);
  const calls = [];
  let allocation = 64;
  const mod = {
    HEAPU8: heap, HEAPU32: new Uint32Array(heap.buffer),
    _malloc(bytes) {
      const pointer = allocation;
      allocation = (allocation + bytes + 3) & ~3;
      return pointer;
    },
    _free() {},
    ccall(name, _type, types, args) {
      if (name === "sdf_layout_freetype_free_string") return;
      calls.push({ name, types: Array.from(types), args: Array.from(args) });
      return 2048;
    },
  };
  const request = { region: "cn", family: "Synthetic", sourceHash: "11".repeat(32), chars: ["A"] };
  const compiled = ts.transpileModule(readFileSync(new URL("../../src/worker.ts", import.meta.url), "utf8"), {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  }).outputText;
  const context = vm.createContext({
    exports: {}, fixtureModule: mod, fixtureRequest: request,
    console, TextDecoder, Uint8Array, performance,
    require(name) {
      assert.equal(name, "./protocol.js");
      return { RENDERER_WORKER_PROTOCOL: "test" };
    },
  });
  vm.runInContext(`${compiled}\nmodulePromise = Promise.resolve(fixtureModule);
    fonts.set(fontKey(fixtureRequest), new ArrayBuffer(1));`, context);
  const prefix = [request.region, request.family, request.sourceHash];
  for (const [kind, options, exportName, suffix] of [
    ["plan", {}, "plan_glyphs_json", ["edt", 2]],
    ["plan", { pointSize: 600 }, "plan_glyphs_at_size_json", ["edt", 2, 600]],
    ["build", { backend: "edt" }, "build_glyph_json_edt", [2]],
    ["build", { backend: "analytic" }, "build_glyph_json", []],
    ["build", { backend: "edt", supersample: 4, pointSize: 600 }, "build_glyphs_at_size_json", [4, 600]],
    ["build", { backend: "analytic", pointSize: 300 }, "build_glyphs_at_size_json", [0, 300]],
    ["map", { pointSize: 600 }, "map_glyphs_json", []],
  ]) {
    const input = { ...request, ...options };
    if (kind === "plan") await context.planGlyphBatch(input);
    else await context.buildGlyphBatch(input, kind === "map");
    const actual = calls.at(-1);
    assert.equal(actual.name, `sdf_layout_freetype_${exportName}`);
    assert.deepEqual(actual.args.slice(4), [...prefix, ...suffix]);
    assert.equal(actual.types.length, actual.args.length);
    assert.equal(heap[actual.args[0]], 0);
    assert.equal(new Uint32Array(heap.buffer)[actual.args[2] >>> 2], 65);
  }
});
