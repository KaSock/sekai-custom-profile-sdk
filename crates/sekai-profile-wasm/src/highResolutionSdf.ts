// FIX-BROWSER-SDF-01: Refine magnified glyphs after layout. Advances, line
// breaks, pivots and material parameters remain those of the original atlas.
import { buildSdfAtlas, type FontSource, type GlyphInfo, type GlyphRequest,
  type PersistentCacheSelection, type SdfAtlas } from "./fontSdfAtlas.js";
import type { RendererWorkerClient } from "./worker-client.js";
import type { GlyphInstance } from "./types/glyph.js";
import type { SemanticDrawOperation } from "./gpu/semanticCommandPlanner.js";

const MIN_MAGNIFICATION = 3;
const MAX_POINT_SIZE = 4096;

/** Pixel-to-atlas ratio, including nonuniform scale, rotation and rich text. */
export function glyphMagnification(instance: GlyphInstance, glyph: GlyphInfo): number {
  if (!instance.drawable || instance.quad.length < 4 || glyph.width <= 0 || glyph.height <= 0) return 0;
  const [a, b, , d] = instance.quad;
  const value = Math.max(Math.hypot(b[0] - a[0], b[1] - a[1]) / glyph.width,
    Math.hypot(d[0] - a[0], d[1] - a[1]) / glyph.height);
  return Number.isFinite(value) ? value : 0;
}

/** Whole-base-size tiers limit cache variants. Leave room for rounded metrics,
 * SDF padding and the atlas packer's one-texel gutter on either side. */
export function highResolutionPointSize(base: SdfAtlas, glyph: GlyphInfo, magnification: number): number {
  if (!(magnification > MIN_MAGNIFICATION) || !Number.isFinite(magnification)) return base.baseSize;
  // Native sampling targets the final device size. Whole-base-size tiers keep
  // cache reuse while avoiding the near-2x oversampling of power-of-two tiers.
  const tier = base.baseSize * Math.ceil(magnification);
  const fit = base.baseSize * Math.min((base.width - 8) / glyph.width, (base.height - 8) / glyph.height);
  return Math.max(base.baseSize, Math.floor(Math.min(tier, MAX_POINT_SIZE, fit)));
}

/** The same font-space alignment as native align_substituted_text_sdf_command.
 * A different rounded bitmap size must not stretch the authored outline. */
export function substituteHighResolutionGlyph(
  instance: GlyphInstance, primary: GlyphInfo, base: SdfAtlas, target: GlyphInfo, sampled: SdfAtlas,
): GlyphInstance {
  const scale = sampled.baseSize / base.baseSize;
  const primaryLeft = Math.floor(primary.planeBearingX) - Math.ceil(base.spread) + 0.5;
  const primaryTop = Math.ceil(primary.planeBearingY) + Math.ceil(base.spread) - 0.5;
  const targetLeft = Math.floor(target.planeBearingX) - Math.ceil(sampled.spread) + 0.5;
  const targetTop = Math.ceil(target.planeBearingY) + Math.ceil(sampled.spread) - 0.5;
  const range = (extent: number, targetExtent: number, offset: number) => {
    const slope = scale * extent / targetExtent;
    const intercept = (offset + 0.5 * (1 - scale)) / targetExtent;
    return [-intercept / slope, (1 - intercept) / slope];
  };
  const [u0, u1] = range(primary.width, target.width, scale * primaryLeft - targetLeft);
  const [v0, v1] = range(primary.height, target.height, targetTop - scale * primaryTop);
  if (![u0, u1, v0, v1].every(Number.isFinite) || scale <= 0 || !target.drawable) return instance;
  const [a, b, , d] = instance.quad;
  const map = (u: number, v: number, tu: number, tv: number): GlyphInstance["quad"][number] => [
    a[0] + (b[0] - a[0]) * u + (d[0] - a[0]) * v,
    a[1] + (b[1] - a[1]) * u + (d[1] - a[1]) * v,
    tu, tv, a[4],
  ];
  const quad = [map(u0, v0, target.u0, target.v0), map(u1, v0, target.u1, target.v0),
    map(u1, v1, target.u1, target.v1), map(u0, v1, target.u0, target.v1)];
  return { ...instance, atlasPage: target.page, quad,
    deviceGlyphQuad: [instance.deviceGlyphQuad[0], quad.map(([x, y]) => [x, y])] };
}

export async function refineHighResolutionSdf(
  base: SdfAtlas,
  instances: GlyphInstance[],
  operations: SemanticDrawOperation[],
  options: { worker: RendererWorkerClient; fonts: FontSource[];
    persistence?: PersistentCacheSelection; supersample?: number; atlasHardPages?: number; signal?: AbortSignal },
): Promise<{ atlas: SdfAtlas; instances: GlyphInstance[] }> {
  // Prebuilt packages can be used without source font bytes and have their
  // own page provider. Never alias their pages with a runtime atlas session.
  if (base.contractId.startsWith("prebuilt:")) return { atlas: base, instances };
  const operationById = new Map(operations.map((op) => [op.command.id, op]));
  const availableFonts = new Set(options.fonts.map((font) => `${font.region}\0${font.sourceHash}\0${font.family}`));
  const demands = new Map<string, { glyph: GlyphInfo; pointSize: number }>();
  const eligible = new Set<GlyphInstance>();
  for (const instance of instances) {
    const glyph = base.glyphs.get(instance.glyphKey);
    const operation = operationById.get(instance.layerId);
    if (!glyph || !operation?.visible || !operation.commandVisible || instance.shaderVertexAlpha <= 0) continue;
    if (!availableFonts.has(`${glyph.region}\0${glyph.fontSourceHash}\0${glyph.family}`)) continue;
    const magnification = glyphMagnification(instance, glyph);
    const pointSize = highResolutionPointSize(base, glyph, magnification);
    if (pointSize <= base.baseSize * MIN_MAGNIFICATION) continue;
    eligible.add(instance);
    const previous = demands.get(instance.glyphKey);
    if (!previous || previous.pointSize < pointSize) demands.set(instance.glyphKey, { glyph, pointSize });
  }
  if (demands.size === 0) return { atlas: base, instances };
  const acquired: SdfAtlas[] = [];
  const replacements = new Map<string, { glyph: GlyphInfo; atlas: SdfAtlas }>();
  let downgraded = 0;
  let skipped = 0;
  try {
    // Resolve one unique font/glyph at a time: each successful page is pinned
    // before another raster can evict it. All tiers share the configured pool.
    // PERF-BROWSER-03: Batch equal font/size demands to amortize worker RPC,
    // planning, hashing and stats. Successful batches pin their pages together.
    // The per-glyph path described above remains the budget fallback: split a
    // failed batch, then try at most one lower tier for an individual glyph.
    type Demand = { key: string; glyph: GlyphInfo; pointSize: number };
    const groups = new Map<string, Demand[]>();
    for (const [key, demand] of demands) {
      const { glyph, pointSize } = demand;
      const groupKey = `${glyph.region}\0${glyph.fontSourceHash}\0${glyph.family}\0${pointSize}`;
      const group = groups.get(groupKey) ?? [];
      group.push({ key, ...demand });
      groups.set(groupKey, group);
    }
    async function resolveBatch(batch: Demand[], pointSize: number): Promise<void> {
      options.signal?.throwIfAborted();
      const requests: GlyphRequest[] = batch.map(({ glyph }) => ({ region: glyph.region,
        family: glyph.family, fontSourceHash: glyph.fontSourceHash, char: glyph.char }));
      let sampled: SdfAtlas;
      try {
        sampled = await buildSdfAtlas(options.fonts, requests, {
          worker: options.worker, persistence: options.persistence, pointSize,
          sessionContractId: base.contractId, atlasHardPages: options.atlasHardPages,
        }, options.supersample, base.sdfBackend);
      } catch (error) {
        if (!String(error).includes("MEMORY_BUDGET_EXCEEDED")) throw error;
        if (batch.length > 1) {
          const middle = Math.ceil(batch.length / 2);
          await resolveBatch(batch.slice(0, middle), pointSize);
          await resolveBatch(batch.slice(middle), pointSize);
        } else {
          const lowerTier = Math.floor((pointSize - 1) / base.baseSize) * base.baseSize;
          if (pointSize === batch[0].pointSize && lowerTier > base.baseSize * MIN_MAGNIFICATION) {
            await resolveBatch(batch, lowerTier);
          } else {
            skipped += 1;
          }
        }
        return;
      }
      acquired.push(sampled);
      for (const { key, pointSize: requestedSize } of batch) {
        const target = sampled.glyphs.get(key);
        if (target?.drawable) {
          replacements.set(key, { glyph: target, atlas: sampled });
          if (pointSize < requestedSize) downgraded += 1;
        }
      }
    }
    for (const group of groups.values()) {
      // Bound temporary raster output to roughly one atlas page and 16 glyphs.
      // An individual large glyph is still allowed; the pool enforces its cap.
      let batch: Demand[] = [];
      let pixels = 0;
      for (const demand of group) {
        const scale = demand.pointSize / base.baseSize;
        const estimate = Math.ceil(demand.glyph.width * scale + 2)
          * Math.ceil(demand.glyph.height * scale + 2);
        if (batch.length && (batch.length >= 16 || pixels + estimate > base.width * base.height)) {
          await resolveBatch(batch, batch[0].pointSize);
          batch = [];
          pixels = 0;
        }
        batch.push(demand);
        pixels += estimate;
      }
      if (batch.length) await resolveBatch(batch, batch[0].pointSize);
    }
    options.signal?.throwIfAborted();
    const refined = instances.map((instance) => {
      const target = replacements.get(instance.glyphKey);
      const primary = base.glyphs.get(instance.glyphKey);
      return eligible.has(instance) && primary && target
        ? substituteHighResolutionGlyph(instance, primary, base, target.glyph, target.atlas) : instance;
    });
    const atlases = [base, ...acquired];
    const last = acquired.at(-1) ?? base;
    console.info(`[sekai-custom-profile-sdk] high-resolution SDF glyphs requested=${demands.size} upgraded=${replacements.size} downgraded=${downgraded} fallback=${skipped} atlas_pages=${last.depth}/${options.atlasHardPages ?? 6}`);
    let released = false;
    return {
      instances: refined,
      atlas: {
        ...base,
        depth: last.depth,
        perf: { ...base.perf,
          totalGlyphCount: atlases.reduce((n, atlas) => n + atlas.perf.totalGlyphCount, 0),
          totalPixelCount: atlases.reduce((n, atlas) => n + atlas.perf.totalPixelCount, 0),
          totalGlyphMs: atlases.reduce((n, atlas) => n + atlas.perf.totalGlyphMs, 0),
          worker: last.perf.worker,
          cache: { ...last.perf.cache,
            hits: atlases.reduce((n, atlas) => n + atlas.perf.cache.hits, 0),
            misses: atlases.reduce((n, atlas) => n + atlas.perf.cache.misses, 0),
            generations: atlases.reduce((n, atlas) => n + atlas.perf.cache.generations, 0),
            sessionHits: atlases.reduce((n, atlas) => n + atlas.perf.cache.sessionHits, 0),
            persistentHits: atlases.reduce((n, atlas) => n + atlas.perf.cache.persistentHits, 0),
            persistentMisses: atlases.reduce((n, atlas) => n + atlas.perf.cache.persistentMisses, 0),
            persistentWritesQueued: atlases.reduce((n, atlas) => n + atlas.perf.cache.persistentWritesQueued, 0),
          },
        },
        release: async () => {
          if (released) return;
          released = true;
          const results = await Promise.allSettled(atlases.map((atlas) => atlas.release()));
          const failure = results.find((result) => result.status === "rejected");
          if (failure?.status === "rejected") throw failure.reason;
        },
      },
    };
  } catch (error) {
    // The caller still owns base on failure. Release every extra lease even
    // if cancellation or a later glyph generation failed.
    await Promise.allSettled(acquired.map((atlas) => atlas.release()));
    throw error;
  }
}
