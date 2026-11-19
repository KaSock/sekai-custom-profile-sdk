import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { createBridge } from "../browser_bot/bridge.mjs";

const original = {
  document: globalThis.document, fetch: globalThis.fetch, FileReader: globalThis.FileReader,
};
afterEach(() => Object.assign(globalThis, original));

function fixture({ animated = false, failDraw = false, missingResource = false, failClassify = false } = {}) {
  const calls = [];
  const gl = {
    VENDOR: 1, RENDERER: 2,
    getExtension: () => ({ UNMASKED_RENDERER_WEBGL: 3 }),
    getParameter: key => key === 3 ? "ANGLE (AMD Radeon RX 580, D3D11)" : "WebKit",
  };
  const canvas = { width: 1830, height: 812, getContext: () => gl };
  const snapshot = {
    getContext: () => ({
      fillRect: () => calls.push("clear"),
      drawImage: () => calls.push("copy"),
    }),
    toBlob: (callback, type, quality) => {
      calls.push(["encode", type, quality]);
      callback(new Blob(["jpeg"], { type }));
    },
  };
  globalThis.document = {
    getElementById: () => canvas, createElement: () => snapshot,
  };
  globalThis.FileReader = class {
    readAsDataURL() {
      this.result = "data:image/jpeg;base64,anBlZw==";
      this.onload();
    }
  };
  globalThis.fetch = async () => new Response("[]", { headers: { "Content-Type": "application/json" } });
  const scene = {
    animated,
    draw() { calls.push("draw"); if (failDraw) throw new Error("context lost"); },
    async destroy() { calls.push("destroy"); },
  };
  let providers;
  const sdk = {
    async create(options) {
      providers = options;
      return {
        async loadMasterData(_revision, load) {
          await load({ table: "test", optional: false }, { signal: new AbortController().signal });
          return { async prepareProfile(request) {
            calls.push(["classify", request]);
            if (failClassify) throw new Error("bad classification");
            return { animationCandidate: animated };
          } };
        },
        async createProfileScene() {
          if (missingResource) {
            await options.resourceProvider.provide(
              { namespace: "assets", key: "card/a b" }, { signal: new AbortController().signal });
          }
          return scene;
        },
      };
    },
  };
  return { sdk, calls, providers: () => providers };
}

const payload = { profile: { userCustomProfileCards: [{ seq: 3 }] }, quality: 90, documentKey: "test" };

test("exports JPEG with the existing BOT envelope and releases the scene", async () => {
  const { sdk, calls } = fixture();
  const bridge = await createBridge(sdk);
  const gpu = await bridge.initialize("cn");
  assert.match(gpu.unmaskedRenderer, /RX 580/);
  const result = await bridge.render(payload);
  assert.equal(result.item.seq, 3);
  assert.equal(result.item.kind, "image");
  assert.equal(result.item.content_type, "image/jpeg");
  assert.equal(result.item.data, "anBlZw==");
  assert.equal(result.item.bytes, 4);
  assert.equal(result.item.backend, "browser");
  assert.deepEqual(calls, ["draw", "clear", "copy", ["encode", "image/jpeg", 0.9], "destroy"]);
});

test("animated scenes are handed back for native export without a still-image substitute", async () => {
  const { sdk, calls } = fixture({ animated: true });
  const bridge = await createBridge(sdk);
  await bridge.initialize("cn");
  const result = await bridge.render(payload);
  assert.equal(result.animated, true);
  assert.equal(result.item, undefined);
  assert.deepEqual(calls, ["destroy"]);
});

test("GPU failure still releases the scene", async () => {
  const { sdk, calls } = fixture({ failDraw: true });
  const bridge = await createBridge(sdk);
  await bridge.initialize("cn");
  await assert.rejects(bridge.render(payload), /context lost/);
  assert.deepEqual(calls, ["draw", "destroy"]);
});

test("classification preserves profile context and skips fonts, images, scenes and GPU work", async () => {
  const { sdk, calls } = fixture({ animated: true });
  const bridge = await createBridge(sdk);
  await bridge.initialize("jp");
  const profile = { userId: "player", userCustomProfileCards: [{ seq: 3 }, { seq: 1 }] };
  const result = await bridge.classify({ profile, documentKey: "classify-test" });
  assert.deepEqual(result.pages, [{ index: 0, animated: true }, { index: 1, animated: true }]);
  assert.equal(calls.length, 2);
  for (const [index, [name, request]] of calls.entries()) {
    assert.equal(name, "classify");
    assert.equal(request.animationDemandOnly, true);
    assert.equal(request.profile.userId, "player");
    assert.deepEqual(request.profile.userCustomProfileCards, [profile.userCustomProfileCards[index]]);
  }
});

test("classification failures leave each page eligible for normal rendering", async () => {
  const { sdk } = fixture({ failClassify: true });
  const bridge = await createBridge(sdk);
  await bridge.initialize("cn");
  const result = await bridge.classify(payload);
  assert.equal(result.pages[0].animated, false);
  assert.match(result.pages[0].error, /bad classification/);
  assert.equal((await bridge.render(payload)).item.kind, "image");
});

test("resource provider uses the local bridge and records unavailable resources", async () => {
  const { sdk, providers } = fixture({ missingResource: true });
  const bridge = await createBridge(sdk);
  await bridge.initialize("cn");
  const urls = [];
  globalThis.fetch = async url => {
    urls.push(url);
    return new Response("", { status: 404 });
  };
  const result = await bridge.render(payload);
  assert.deepEqual(urls, ["./resource/assets/card/a%20b"]);
  assert.equal(Object.hasOwn(providers(), "token"), false);
  assert.deepEqual(result.item.warnings, ["Browser resource unavailable: assets/card/a b"]);
});
