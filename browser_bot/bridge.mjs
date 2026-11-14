// BROWSER-01: Keep one SDK worker, canvas and decoded-image cache per browser slot.
// The Python bridge owns BOT credentials; this page only uses relative URLs.
export async function createBridge(BrowserRenderer) {
  const canvas = document.getElementById("card");
  const snapshot = document.createElement("canvas");
  const missing = new Set();
  let renderer;
  let masterData;
  let gpu;

  async function initialize(region) {
    renderer = await BrowserRenderer.create({
      canvas, region,
      fontProvider: {
        async provide(request, { signal }) {
          const response = await fetch("./font?family=" + encodeURIComponent(request.family), { signal });
          if (!response.ok) throw new Error("Font unavailable: " + request.family);
          return { bytes: await response.arrayBuffer() };
        },
      },
      resourceProvider: {
        async provide(resource, { signal }) {
          const key = resource.key.split("/").map(encodeURIComponent).join("/");
          const url = "./resource/" + encodeURIComponent(resource.namespace) + "/" + key;
          const response = await fetch(url, { signal });
          if (!response.ok) {
            missing.add(resource.namespace + "/" + resource.key);
            return null;
          }
          return { source: await response.blob() };
        },
      },
    });
    // Let the SDK choose context attributes before inspecting its actual GPU.
    const gl = canvas.getContext("webgl2");
    if (!gl) throw new Error("WEBGL2_UNAVAILABLE");
    const extension = gl.getExtension("WEBGL_debug_renderer_info");
    gpu = {
      vendor: gl.getParameter(gl.VENDOR),
      renderer: gl.getParameter(gl.RENDERER),
      unmaskedRenderer: extension ? gl.getParameter(extension.UNMASKED_RENDERER_WEBGL) : null,
    };
    masterData = await renderer.loadMasterData("bot-local", async ({ table, optional }, { signal }) => {
      const response = await fetch("./masterdata/" + encodeURIComponent(table), { signal });
      if (optional && response.status === 404) return null;
      if (!response.ok) throw new Error("MasterData unavailable: " + table);
      return response.json();
    });
    return gpu;
  }

  async function render({ profile, quality, documentKey }) {
    if (!renderer || !masterData) throw new Error("Browser slot has not initialized");
    missing.clear();
    const started = performance.now();
    let scene;
    try {
      scene = await renderer.createProfileScene({
        masterData, profile, documentKey, pageIndex: 0, frameMode: "animate",
        // The browser stays alive across requests; do not imply disk persistence.
        sdf: { persistence: "memory-only" },
      });
      const prepared = performance.now();
      const timing = { prepare_ms: Math.round(prepared - started) };
      if (scene.animated) {
        timing.total_ms = Math.round(performance.now() - started);
        return { animated: true, timing };
      }
      // drawImage immediately follows draw in the same task. The SDK uses
      // preserveDrawingBuffer=false; an intervening await could clear the canvas.
      scene.draw();
      snapshot.width = canvas.width;
      snapshot.height = canvas.height;
      const context = snapshot.getContext("2d");
      if (!context) throw new Error("Canvas 2D is required for JPEG export");
      context.fillStyle = "#fff";
      context.fillRect(0, 0, snapshot.width, snapshot.height);
      context.drawImage(canvas, 0, 0);
      const copied = performance.now();
      const blob = await new Promise((resolve, reject) => {
        snapshot.toBlob(value => value ? resolve(value) : reject(new Error("JPEG export failed")),
          "image/jpeg", quality / 100);
      });
      if (blob.type !== "image/jpeg") throw new Error("Browser does not support JPEG export");
      if (blob.size > 64 * 1024 * 1024) throw new Error("Encoded page exceeds 64 MiB");
      const dataUrl = await new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(reader.result);
        reader.onerror = () => reject(new Error("Could not read encoded JPEG"));
        reader.readAsDataURL(blob);
      });
      timing.draw_copy_ms = Math.round(copied - prepared);
      timing.encode_transfer_ms = Math.round(performance.now() - copied);
      timing.total_ms = Math.round(performance.now() - started);
      return {
        animated: false, timing,
        item: {
          seq: profile.userCustomProfileCards[0].seq,
          kind: "image", content_type: "image/jpeg", bytes: blob.size,
          data: dataUrl.slice(dataUrl.indexOf(",") + 1), backend: "browser",
          warnings: [...missing].map(key => "Browser resource unavailable: " + key),
          timing,
        },
      };
    } finally {
      if (scene) await scene.destroy();
    }
  }

  return { initialize, render };
}
